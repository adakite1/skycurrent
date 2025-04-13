#ifndef LMQ_H
#define LMQ_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum lmq_action_t {
  LMQ_ACTION_CONTINUE = 0,
  LMQ_ACTION_PAUSE = 1,
  LMQ_ACTION_DEREGISTER = -1,
} lmq_action_t;

typedef enum lmq_resume_mode_t {
  LMQ_RESUME_WAIT = 0,
  LMQ_RESUME_TRY = 1,
} lmq_resume_mode_t;

/**
 * Linked message queue.
 */
typedef struct lmq_t lmq_t;

/**
 * Message consumer.
 * 
 * Each instance should be used by a single thread at a time. Create multiple consumers if multi-threading in that way is necessary.
 */
typedef struct lmq_consumer_t lmq_consumer_t;

/**
 * Message.
 * 
 * Each instance should be used by a single thread at a time.
 */
typedef struct lmq_message_t lmq_message_t;

/**
 * Message reference. Keeps a peeked message's pointer valid for the duration of its existence.
 */
typedef struct lmq_message_ref_t lmq_message_ref_t;

/**
 * Vector of bytes.
 */
typedef struct lmq_vec_t lmq_vec_t;

/**
 * Message callback function type.
 */
typedef lmq_action_t (*lmq_msg_callback_t)(lmq_message_t *message, void *user_data);

/**
 * 64-bit unsigned integer identifying a registered handler.
 */
typedef uint64_t lmq_msg_callback_id;

/**
 * Create a new linked message queue.
 * 
 * The returned queue must be freed with `lmq_destroy` after usage.
 */
lmq_t *lmq_new(void);

/**
 * Destroy queue.
 */
void lmq_destroy(lmq_t *queue);

/**
 * Push a message into the end of the queue.
 */
void lmq_push(lmq_t *queue, const uint8_t *data, size_t len);

/**
 * Create a new message consumer which consumes from the provided queue.
 * 
 * The returned consumer must be freed with `lmq_consumer_destroy` after usage.
 */
lmq_consumer_t *lmq_consumer_new(const lmq_t *queue);

/**
 * Destroy consumer.
 */
void lmq_consumer_destroy(lmq_consumer_t *consumer);

/**
 * Attempt to return the next unclaimed message without blocking.
 * 
 * Returns `NULL` if no new unclaimed messages are available.
 * 
 * All previously obtained messages must either be claimed, destroyed, or have their peek views released before this function will unblock.
 * 
 * The returned message must be freed with `lmq_message_destroy` after usage.
 */
lmq_message_t *lmq_consumer_try_next(lmq_consumer_t *consumer);

/**
 * Block until the next unclaimed message arrives.
 * 
 * All previously obtained messages must either be claimed, destroyed, or have their peek views released before this function will unblock.
 * 
 * The returned message must be freed with `lmq_message_destroy` after usage.
 */
lmq_message_t *lmq_consumer_wait(lmq_consumer_t *consumer);

/**
 * Register an `lmq_msg_callback_t` handler function that will be called whenever a new unclaimed message is available.
 * 
 * Each handler is associated with an `lmq_consumer_t`. Users MUST avoid using or freeing the message consumer while the handler is registered, as the consumer will be used by the handler internally.
 * 
 * Due also to the handler using the consumer behind-the-scenes, registering multiple callbacks using the same message consumer is undefined behavior! Message consumers are not thread-safe, so there will be data races. However there are no checks for if the given callback has already been registered using a given consumer already, so users should take care to avoid doing so.
 * 
 * All messages provided to the handler must either be claimed (`lmq_message_claim`), destroyed (`lmq_message_destroy`), or have their peek views released (`lmq_message_peek_release`) before the handler will be guaranteed a call with the next message.
 * 
 * Callback should return an `lmq_action_t` which can be used by the callback to either pause itself or immediately deregister. Paused handlers can be resumed by `lmq_resume_handler`.
 * 
 * If `TRUE` is passed for `start_paused`, the callback can also be paused immediately on start before processing a single message.
 * 
 * Given callback must be thread-safe as internally handlers run on a separate thread controlled by the tokio runtime. However a callback will not be called simultaneously from multiple threads, only possibly in succession.
 * 
 * `user_data` is passed *as-is* to the callback; it is the user's responsibility to make sure that the `user_data` pointer is thread-safe.
 * 
 * Returns a `uint64_t` identifier for the registered handler which can be used to resume or deregister the handler.
 * 
 * Only available when compiling with feature "tokio".
 */
lmq_msg_callback_id lmq_register_handler(lmq_consumer_t *consumer, lmq_msg_callback_t callback, bool start_paused, void *user_data);

/**
 * Resume a handler currently paused from `LMQ_ACTION_PAUSE`.
 * 
 * If `no_block` is 0 the handler will resume waiting as usual; a value of 1 though will cause the handler to try to get a message and immediately call the callback with the result. If a message is not available on that attempt, the callback will still be called but with NULL for `message`. The callback will then continue to be called until the callback explicitly requests to be paused again; the user is advised to pause eventually, as until then the thread backing the handler will busy-wait.
 * 
 * The handler will resume on the message after the one it paused on, and no messages arrived while the handler was paused will be lost.
 * 
 * Calling `lmq_resume_handler` more than once will be interpreted as multiple resume requests and will block the current thread on the second call since the handler will not be paused.
 * 
 * Returns 0 on success and -1 if the handler is no longer running and cannot be resumed.
 */
int lmq_resume_handler(lmq_msg_callback_id callback_id, int no_block);

/**
 * Deregister a handler function.
 * 
 * Returns `TRUE` if a handler was successfully deregistered. If `FALSE` is returned, the provided handler was not registered with the given queue.
 */
bool lmq_deregister_handler(lmq_msg_callback_id callback_id);

/**
 * Destroy message.
 */
void lmq_message_destroy(lmq_message_t *message);

/**
 * Read the message without claiming it.
 * 
 * The pointer and length of the message's data will be written into `p` and `len` respectively, while the return value is the handle for the message read lock.
 * 
 * `p` is only guaranteed to remain valid for as long as the returned `lmq_message_ref_t` and the original `lmq_message_t` remains intact.
 * 
 * It is the user's responsibility to destroy the lock with `lmq_message_peek_release` after they are done with the data.
 * 
 * Returns `NULL` and writes null into `p` and `len` if the message has since been claimed.
 */
lmq_message_ref_t *lmq_message_peek(const lmq_message_t *message,
                                    const uint8_t **p,
                                    size_t *len);

/**
 * Destroy message read lock.
 */
void lmq_message_peek_release(lmq_message_ref_t *message_ref);

/**
 * Claim the message and return the payload.
 * 
 * The pointer and length of the message's data will be written into `p` and `len` respectively, while the return value is the handle for the underlying vector structure.
 * 
 * `p` is guaranteed to remain valid for as long as the returned `lmq_vec_t` remains intact.
 * 
 * The returned vector must be freed with `lmq_vec_destroy` after usage.
 * 
 * Returns `NULL` and writes null into `p` and `len` if the message has since been claimed.
 */
lmq_vec_t *lmq_message_claim(lmq_message_t *message, uint8_t **p, size_t *len);

/**
 * Destroy vector.
 */
void lmq_vec_destroy(lmq_vec_t *data);

#ifdef __cplusplus
}
#endif

#endif

