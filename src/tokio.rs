use std::{path::{Path, PathBuf}, sync::{Arc, OnceLock}};
use parking_lot::{Condvar, Mutex, RwLock};
use tokio::sync::mpsc;
use std::thread;

pub use tokio::sync::mpsc::error::SendError;

use crate::{init, send_stream, shutdown_signal, InitFlags};

static GLOBAL_PROJECT_DIR: OnceLock<Arc<Mutex<Option<PathBuf>>>> = OnceLock::new();
static GLOBAL_STREAM_SENDER: OnceLock<Arc<Mutex<SkyCurrentStreamSender>>> = OnceLock::new();

/// Sets the global project directory.
pub fn set_global_project_dir(project_dir: impl AsRef<Path>) {
    let project_dir = project_dir.as_ref();
    let lock = GLOBAL_PROJECT_DIR.get_or_init(|| Arc::new(Mutex::new(None)));
    *lock.lock() = Some(project_dir.to_path_buf());
}

enum SendThreadMessage {
    Send {
        payload: Vec<u8>,
        header_size: usize
    },
    ShutdownSignal,
    Terminate
}
pub struct SkyCurrentStreamSender {
    sender_tx: mpsc::Sender<SendThreadMessage>,
    sender_termination_pair: Arc<(Mutex<bool>, Condvar)>,
}
impl SkyCurrentStreamSender {
    fn is_closed(&self) -> bool {
        self.sender_tx.is_closed()
    }
    /// Creates a brand new stream sender with its own sending thread.
    fn new(project_dir: impl AsRef<Path>) -> Self {
        let project_dir = project_dir.as_ref().to_path_buf();

        // Channels for communication between threads and async context
        let (sender_tx, mut sender_rx) = mpsc::channel::<SendThreadMessage>(100);
        let sender_termination_pair = Arc::new((Mutex::new(false), Condvar::new()));

        // Spawn sender thread
        let send_dir = project_dir;
        let sender_termination_pair_thread = sender_termination_pair.clone();
        thread::spawn(move || {
            if let Err(e) = init(&send_dir, InitFlags::IOX2_CREAT_PUBLISHER | InitFlags::IOX2_CREAT_NOTIFIER) {
                panic!("Failed to initialize sender thread: {:?}", e);
            }
            loop {
                match sender_rx.blocking_recv() {
                    Some(message) => match message {
                        SendThreadMessage::Send { payload, header_size } => {
                            if let Err(e) = send_stream(&payload, header_size) {
                                panic!("Error in sender thread: {:?}", e);
                            }
                        },
                        SendThreadMessage::ShutdownSignal => {
                            if let Err(e) = shutdown_signal() {
                                panic!("Error in sender thread: {:?}", e);
                            }
                        },
                        SendThreadMessage::Terminate => break,
                    },
                    None => break,
                }
            }
            // Do this right before exiting to notify any on-lookers that the sender thread has exited.
            let (lock, cvar) = &*sender_termination_pair_thread;
            let mut exited = lock.lock();
            *exited = true;
            cvar.notify_all();
        });

        Self {
            sender_tx,
            sender_termination_pair,
        }
    }
    /// Get the global stream sender if it exists or initializes a new one if it doesn't.
    /// 
    /// Note that `get_or_init` will always block and only return once the global stream sender is in a ready state.
    /// 
    /// # Panics
    /// 
    /// Panics if `set_global_project_dir` has not been called beforehand! Use it to set the global project directory first.
    pub fn get_or_init() -> parking_lot::lock_api::MutexGuard<'static, parking_lot::RawMutex, SkyCurrentStreamSender> {
        let project_dir;
        if let Some(global_project_dir) = GLOBAL_PROJECT_DIR.get() {
            if let Some(global_project_dir) = global_project_dir.lock().as_ref() {
                project_dir = global_project_dir.clone();
            } else {
                panic!("call `set_global_project_dir` first! (2)");
            }
        } else {
            panic!("call `set_global_project_dir` first! (1)");
        }
        let lock = GLOBAL_STREAM_SENDER.get_or_init(|| Arc::new(Mutex::new(
            SkyCurrentStreamSender::new(&project_dir)
        )));

        // Get a reference to the existing global_stream_sender.
        let mut global_stream_sender = lock.lock();

        // Check if the stream sender is still open. If it is, we can stop here and just return the current global stream sender.
        if !global_stream_sender.is_closed() {
            return global_stream_sender;
        }

        // If the existing sender is closed, create a new one.
        *global_stream_sender = SkyCurrentStreamSender::new(project_dir);
        global_stream_sender
    }
    /// Send termination signal to stream sender and then wait for it to close.
    pub async fn close(&self) -> tokio::io::Result<()> {
        // Send termination signal to thread.
        match self.sender_tx.send(SendThreadMessage::Terminate).await {
            Ok(_) => {
                // Sender thread hasn't exited yet, but the signal has been sent, so the thread should eventually see it and exit. Wait for that to happen.
                let (lock, cvar) = &*self.sender_termination_pair;
                let mut exited = lock.lock();
                if !*exited {
                    cvar.wait(&mut exited);
                }
            },
            Err(_) => {
                // Sender thread has already exited.
            },
        }
        Ok(())
    }
    /// Send a stream.
    /// 
    /// Note that since this requires assembly of data on the receiver's side, all payloads should have a small header section so that receivers can decide if they want to reconstruct the message or pass on it, saving memory and execution time.
    /// 
    /// The `header_size` specifies how many bytes from the head of the payload corresponds to the header; every fragment sent will include the header but have different sections of the body. The receiver can then filter incoming messages based on the header, not copying messages it does not need.
    pub async fn send_stream(&self, payload: Vec<u8>, header_size: usize) -> Result<(), SendError<Vec<u8>>> {
        self.sender_tx.send(SendThreadMessage::Send{ payload, header_size }).await
            .map_err(|e| if let SendThreadMessage::Send { payload, header_size: _ } = e.0 { SendError(payload) } else { panic!("this should never happen.") })
    }
}

/// A message queue based upon a linked list so that consuming iterators can decide to process any message atomically and independent of other consumers, who might be looking at a different set of messages.
pub struct LinkMessageQueue {
    root: Arc<Mutex<NextMessage>>,
    last: Arc<Mutex<NextMessage>>,
    tail: Arc<Mutex<NextMessage>>,
    new_message_pair: Arc<(Mutex<u8>, Condvar)>,
}
/// Smart pointer referencing the `Vec<u8>` payload inside of a stored element, guarded by a mutex for a temporary view of the data.
pub struct MessageRef<'a> {
    guard: parking_lot::lock_api::RwLockReadGuard<'a, parking_lot::RawRwLock, Option<Vec<u8>>>,
}
impl<'a> std::ops::Deref for MessageRef<'a> {
    type Target = Vec<u8>;

    fn deref(&self) -> &Self::Target {
        // [1] Safe because we only create `MessageRef` objects on elements with a confirmed `Some` payload.
        self.guard.as_ref().unwrap()
    }
}
/// Represents an element in the unclaimed messages linked list.
/// 
/// If the `payload` is `None`, that element is either the root element or a claimed element.
/// 
/// Each client iterating through these messages should keep their current element lock while also locking the next element. If the next element is *not* the root element and is empty (indicating it has been claimed), it should recycle that element by taking the `next` parameter on that element and placing it inside the still-held current element's `next` parameter instead, basically skipping that claimed element for future iterators.
#[derive(Clone)]
pub struct NextMessage {
    payload: Arc<RwLock<Option<Vec<u8>>>>,
    next: Option<Arc<Mutex<NextMessage>>>
}
impl NextMessage {
    fn is_claimed(&self) -> bool {
        self.payload.read().is_none()
    }
    pub fn read(&self) -> Option<MessageRef> {
        let lock = self.payload.read();
        if lock.is_some() {
            Some(MessageRef {
                guard: self.payload.read(),
            })
        } else {
            None
        }
    }
    pub fn claim(&mut self) -> Option<Vec<u8>> {
        self.payload.write().take()
    }
}
impl LinkMessageQueue {
    pub fn new() -> Self {
        // Create empty tail node (serving as sentinel, indication is next=None)
        let tail = NextMessage {
            payload: Arc::new(RwLock::new(None)),  // Tail has no payload.
            next: None,
        };
        let tail_arc = Arc::new(Mutex::new(tail));

        // Create empty root node (serving as sentinel, indication is payload=None)
        let root = NextMessage {
            payload: Arc::new(RwLock::new(None)),  // Root has no payload.
            next: Some(tail_arc.clone()),
        };
        let root_arc = Arc::new(Mutex::new(root));

        Self {
            root: root_arc.clone(),
            last: root_arc,
            tail: tail_arc,
            new_message_pair: Arc::new((Mutex::new(0), Condvar::new())),
        }
    }

    pub fn push(&mut self, payload: Vec<u8>) {
        // Create new message node.
        let new_node = Arc::new(Mutex::new(NextMessage {
            payload: Arc::new(RwLock::new(Some(payload))),
            next: Some(self.tail.clone()),
        }));

        {
            let mut current_last = self.last.clone();

            // Immediately lock the last node to prevent pruning, if the node has not yet been pruned.
            let mut current_lock = current_last.lock();

            // [2] IMPORTANT CHECKS
            // Here there are two possibilities. The lock has prevented the last node on record from being pruned for the time being, if it hasn't yet been pruned already.
            // Thus we can check if the node has been pruned or not.
            // We will know if it has been pruned if the `next` pointer no longer points towards our sentinel tail node.
            //  If it doesn't point to the sentinel tail, we interpret the node it points towards as the new suggested last node.
            //  Then we recursively follow the `next` pointer until we reach a node that points towards the sentinel tail.
            //  At which point, we will adopt that node as the new last node.
            // If the node has not been pruned yet, or aka, the `next` pointer is correct, then we simply use that node regardless of whether its payload is empty or not.
            // The next pruning will correct things.

            // Keep traversing until we find the new node that points to the tail.
            while let Some(next_arc) = &current_lock.next {
                // Check node.
                if Arc::ptr_eq(next_arc, &self.tail) {
                    break;  // Found the true last node.
                }

                // Not pointing to the tail, so we move on to the next node.
                // Drop the current one, then acquire the next one.
                let next_clone = next_arc.clone();
                drop(current_lock);  // Drop the previous lock early.

                current_last = next_clone;
                current_lock = current_last.lock();
            }

            // At this point, `current_lock` is the true last node.
            // Update its `next` pointer to our new node to add it into the list.
            current_lock.next = Some(new_node.clone());
        }

        // Update last to the new node.
        self.last = new_node;

        // Notify waiting consumers.
        let (lock, cvar) = &*self.new_message_pair;
        let mut wrapping_counter = lock.lock();
        *wrapping_counter = wrapping_counter.wrapping_add(1);
        cvar.notify_all();
    }

    pub fn create_consumer(&self) -> MessageConsumer {
        MessageConsumer {
            current: self.root.clone(),
            new_message_pair: self.new_message_pair.clone(),
        }
    }
}
/// Linked-list message queue iterator.
/// 
/// Each unclaimed message is visited once and in order of arrival.
pub struct MessageConsumer {
    current: Arc<Mutex<NextMessage>>,
    new_message_pair: Arc<(Mutex<u8>, Condvar)>,
}
impl MessageConsumer {
    pub fn try_next(&mut self) -> Option<NextMessage> {
        let mut return_value = None;
        let mut move_to = None;

        // Here, as we iterate through the linked list, we also prune it by changing the current element's `next` point to the next unclaimed element, thereby skipping them.
        // This presents a challenge however, since the linked list itself might also need to push new items, and if the current last item is pruned, then any new messages will never reach consumers since the chain is broken.
        // There are a few scenarios then:
        // - The list's `last` is currently the root node.
        //   If this is true, then there's nothing to worry about.
        // - The list's `last` is currently one of the message nodes.
        //   In this case, there are 3 more sub-scenarios:
        //   - The list's `last` is unclaimed.
        //     Since the `last` is as yet unclaimed, it will never be skipped by the pruning algorithm.
        //   - The list's `last` *is* claimed already, but hasn't been pruned yet, and the linked list adds a new element after it.
        //     The linked list, in order to write this information in, needs to lock the unpruned element. Thus, we need to make sure that first of all, any pruning to skip over any node should also require a mutex lock so that once the `push` routine is inserting a new node, it won't have to deal with the node it believes to be last being pruned suddenly.
        //     This way, any pruning of that node is *guaranteed* to happen either before [2] or after it has finished inserting the new node, not during.
        //     With that invariant ensured, we no longer have to consider this case.
        //   - The list's `last` *is* claimed already, and *has* been pruned already.
        //     Since the list structure only knows about the `last` node as it knew some time before, that node might have been pruned already.
        //     So the challenge becomes, how will the list know what the new `last` node is?
        //     This is where we can help the list structure by, when pruning, setting every node we prune's `next` to point to the node we *know* is valid, or, the node we're starting the pruning from.
        //     By doing so we can also subtly inform the list structure methods that it *was* pruned, because the list structure will expect the `last` node to point to the tail sentinel node, but once it sees that it no longer points to that,
        //     it will know that the node is no longer considered part of the list structure, and it can also get the hint that it needs to follow the `next` pointers until it reaches a node that *does* point to the tail sentinel.

        // [3] Theory
        // During pruning, current node *CANNOT* be claimed. Lock it for the duration.
        // If current node is already claimed however, then do *NOT* update subsequent nodes' `next` addresses to self (aka, build a subtree pointing to self).
        // Only traverse the tree until we reach the next proper node and update self so as to build a shortcut from self to next valid node.
        // This ensure that:
        // - Each prune creates a subtree of *ONLY* the type where dead nodes point towards a node that *was* guaranteed to be alive at the time of pruning.
        // - Each prune also only creates shortcuts that reduce the level of separation from the current node to the main link.
        // Furthermore, these invariants only hold when the entire pruning operation is done in one go!
        // This means that the subtree is built in one go, and ensures that only once the subtree is built are other consumers allowed to consume the pointers, 
        // avoiding any circular references that might occur.
        // In practice, this *SHOULD* mean that when pruning, every lock created on intermediate nodes must be held until the pruning is done, but it is easy to see 
        // that Rust will not allow us to hold an arbitrary vector of a bunch of locks.
        // Thus, we must work with what we have.
        // [4]
        // On each iteration, we hold two locks, one on the current node and one on a node somewhere beyond the current one.
        // We must then make sure that each iteration ends with a valid subtree with no recursion.
        // The main pain point in achieving that is that, say we are node A, and the list is A, B, C, D.
        // Initially: A => B, B => C, C => D
        // Say B and C needs pruning.
        // A => B, B => A, C => D
        // then
        // A => B, B => A, C => A
        // then
        // A => D, B => A, C => A
        // As we can see, in between the tree becomes self-referencing, which is not what we want.
        // Instead, we can do the following:
        // Initially: A => B, B => C, C => D
        // A => C, B => A, C => D
        // In other words, notice that on that iteration, B gives us the destination we will visit on the next iteration of the loop, C.
        // B *would* have visited C anyways, but now we have it pointed to us, so we point ourselves to C.
        // And all that is to say, we build our very own subtree pointing to us one node at a time, each time making sure that the subtree's exit, us,
        // does not point to any of the new entrants into our subtree but instead pointing to a new potential recruit. This way, the tree is valid in between 
        // lock changes, and only transforms while no one else has the locks to see what we are doing. And when they get their own locks, the tree they see is 
        // once again valid.

        {
            // Lock the current node.
            let mut current = self.current.lock();
            // Lock the current node's message, as per [3]. Use the strictest lock (write lock) so that during the pruning operation, the current node cannot be claimed. Technically a read lock also works here but this is an important invariant.
            let current_payload = current.payload.clone();
            let current_payload_rw_lock = current_payload.write();
            // Then check the claim status.
            let current_is_claimed = current_payload_rw_lock.is_none();

            // Get a copy of the next node.
            let mut next = current.next.clone();

            while let Some(next_arc) = &next {
                // Immediately lock the next node to prevent others from modifying it.
                let mut next_lock = next_arc.lock();

                // Now there are a few possibilities:
                // - The next node is the tail sentinel, in which case there's no pruning to do. In fact, given that we were able to reach this point, we are now the new last node. Break early.
                // - The next node is a regular unclaimed node, in which case no further pruning should be done. In this case we are not the new last node. Break early.
                //   One thing to note is that even though in this case we are not the new last node, it still makes sense to pretend to be so in the previous iterations 
                //   of this loop since we won't know until we finish pruning, and when we reach the point where we know if we're the new last node it will be too late and 
                //   very non-performant to go back and fix all their pointers to us, not to mention the problem of trying to get the Mutex locks again on nodes that might 
                //   have been dropped already.
                //   In effect, we are setting all the `next` fields on the pruned nodes to the "earliest still valid node".
                if !next_lock.is_claimed() || next_lock.next.is_none() {
                    // Either we have reached the tail sentinel or the next unclaimed node.
                    // Now we can finish up.
                    // Set our own `next` to this node.
                    current.next = Some(next_arc.clone());

                    if let Some(_) = &next_lock.next {
                        // Next node is regular unclaimed node, as seen by the presence of a `next` value on it. Clone the message within for return.
                        return_value = Some(next_lock.clone());

                        // Set move forward destination.
                        move_to = Some(next_arc.clone());
                    } // Otherwise the next node is tail sentinel, so we shouldn't move to it, and our return value is already `None` by default so no need to change it.

                    break;
                } else {
                    // To fulfill invariant [4].
                    if let Some(next_next_arc) = &next_lock.next {
                        current.next = Some(next_next_arc.clone());
                    }
                }
                // - The next node's payload has already been claimed, but it has not yet been pruned.
                //   If so, we can prune it by updating it to point to us.
                // - The next node's payload has already been claimed, and it has already been pruned.
                //   If so, what must have happened was that one of the nodes we have already visited was originally the one 
                //   to prune it, but then it got claimed itself as well, meaning it can no longer serve as the new last node.
                //   Thus, given that we were still able to reach this node, it makes sense for us to update the `next` with our 
                //   own address, just in case it is still pointed to by the list's current internal `last` record a few links in, to provide a quicker line to the true 
                //   last node.

                // Clone the next node.
                let new_next = next_lock.next.clone();

                // Since we're pruning this node now, set its `next` to the "earliest still valid node" which is us.
                // But ONLY DO SO if we ourselves have not been claimed, and thus we ourselves are not eligible for pruning. See [3].
                // [5] This is perhaps the tiny block of code that must be watched the most carefully, because this is the only part 
                // of the graph building process where we can introduce cyclic links. The other places where we modify the graph are when 
                // we introduce a new node (which, since it adds a new node, will never create cyclic links) and when we point ourselves 
                // (current) towards the next valid node we find (which just makes a shortcut to another valid node further down the tree).
                if !current_is_claimed {
                    next_lock.next = Some(self.current.clone());
                }

                // Drop, then move to new next.
                drop(next_lock);  // Drop the previous lock early.

                next = new_next;
            }
        }

        // Move the consumer forward if a destination was set.
        if let Some(dest) = move_to {
            // Move.
            self.current = dest;
        }

        return_value
    }

    pub fn next_blocking(&mut self) -> NextMessage {
        loop {
            // Try to get the next message.
            if let Some(payload) = self.try_next() {
                return payload;
            }
            
            // No messages available, wait at the current position.
            let (lock, cvar) = &*self.new_message_pair;
            let mut wrapping_counter = lock.lock();
            cvar.wait(&mut wrapping_counter);
        }
    }
}

