import ffi, { lmq_action_t, lmq_resume_mode_t } from "./src/ffi.ts";

const lmq_registry = typeof FinalizationRegistry !== "undefined" ? 
  new FinalizationRegistry((inner: Deno.PointerValue) => {
    ffi.lmq_destroy(inner);
  }) : undefined;

/**
 * Linked message queue.
 * 
 * Must be freed with the `free` method after use.
 */
export class LinkMessageQueue {
  #_inner: Deno.PointerValue;

  constructor() {
    this.#_inner = ffi.lmq_new();
    lmq_registry?.register(this, this.#_inner, this);
  }

  free(): void {
    if (this.#_inner) {
      lmq_registry?.unregister(this);
      ffi.lmq_destroy(this.#_inner);
      this.#_inner = null;
    }
  }

  /**
   * Push a message into the end of the queue.
   * 
   * @param data `Uint8Array` to be pushed into the end of the queue.
   */
  push(data: Uint8Array): void {
    ffi.lmq_push(this.#_inner, data, BigInt(data.byteLength));
  }

  /**
   * Create a new message consumer which consumes from the queue.
   * 
   * @returns `MessageConsumer` which can be used to consume messages from the queue.
   */
  create_consumer(): MessageConsumer {
    return MessageConsumer._createConsumerFromQueuePointer(this.#_inner);
  }
}

class MessageCallbackId {
  queue: Deno.PointerValue;
  callback: Deno.PointerObject;

  constructor(queue: Deno.PointerValue, callback: Deno.PointerObject) {
    this.queue = queue;
    this.callback = callback;
  }
}

const lmq_msg_callback_registry = typeof FinalizationRegistry !== "undefined" ? 
  new FinalizationRegistry(({queue, callback}: MessageCallbackId) => {
    ffi.lmq_deregister_handler(queue, callback);
  }) : undefined;

/**
 * Message consumer.
 * 
 * Must be freed with the `free` method after use.
 */
export class MessageConsumer {
  #_callback?;
  #_callbackId?;
  #_resolve?: (value: (NextMessage | null) | PromiseLike<(NextMessage | null)>) => void;

  private constructor(queue: Deno.PointerValue) {
    this.#_callback = Deno.UnsafeCallback.threadSafe(
      {
        parameters: [
          "pointer", // lmq_message_t *message
          "pointer", // void *user_data
        ],
        result: "i32", // lmq_action_t
      },
      (message, _user_data) => {
        if (this.#_resolve) {
          this.#_resolve(message ? NextMessage._createMessageFromRaw(message) : null);
          this.#_resolve = undefined;
          return lmq_action_t.LMQ_ACTION_PAUSE;
        } else {
          throw new Error("lmq internal error: message consumer no promises to give message to! this should never happen.");
        }
      }
    );
    this.#_callbackId = new MessageCallbackId(queue, this.#_callback.pointer);
    ffi.lmq_register_handler(queue, this.#_callback.pointer, true, null);
    lmq_msg_callback_registry?.register(this, this.#_callbackId, this);
  }

  static _createConsumerFromQueuePointer(queue: Deno.PointerValue): MessageConsumer {
    return new MessageConsumer(queue);
  }

  free(): void {
    if (this.#_callback && this.#_callbackId) {
      lmq_msg_callback_registry?.unregister(this);
      ffi.lmq_deregister_handler(this.#_callbackId.queue, this.#_callbackId.callback);
      this.#_callbackId = undefined;
      this.#_callback = undefined;
    }
  }

  /**
   * Asynchronously wait for the next message to arrive.
   * 
   * Only one `Promise` returned by either `next` or `try_next` can be unresolved at one time. An error will be thrown if this is not followed.
   * 
   * @returns `Promise` that resolves with a new message.
   */
  next(): Promise<NextMessage> {
    return this._proceed(lmq_resume_mode_t.LMQ_RESUME_WAIT) as Promise<NextMessage>;
  }

  /**
   * Attempt to return the next unclaimed message without blocking.
   * 
   * Only one `Promise` returned by either `next` or `try_next` can be unresolved at one time. An error will be thrown if this is not followed.
   * 
   * While the return value is still a `Promise`, unlike the promise returned by `next` the promise returned here should resolve almost instantly as it doesn't actually wait under-the-hood.
   * 
   * @returns `Promise` that resolves with either a new message or `null` if none is immediately available.
   */
  try_next(): Promise<NextMessage | null> {
    return this._proceed(lmq_resume_mode_t.LMQ_RESUME_TRY) as Promise<NextMessage | null>;
  }

  private _proceed(no_block: number): Promise<NextMessage | (NextMessage | null)> {
    if (this.#_resolve) {
      throw new Error("wait until the previous call to `next` or `try_next` resolves before calling either again!");
    }
    return new Promise((resolve, reject) => {
      this.#_resolve = resolve;
      try {
        if (this.#_callbackId) {
          let error_code: number;
          if ((error_code = ffi.lmq_resume_handler(this.#_callbackId.queue, this.#_callbackId.callback, no_block)) !== 0) {
            reject(new Error(`lmq internal error: \`lmq_resume_handler\` error code ${error_code}. handler is no longer running and cannot be resumed! this should never happen.`));
          }
        } else {
          reject(new Error("no callbacks registered! this should never happen."));
        }
      } catch (e) {
        reject(e);
      }
    });
  }
}

const lmq_message_registry = typeof FinalizationRegistry !== "undefined" ? 
  new FinalizationRegistry((inner: Deno.PointerValue) => {
    ffi.lmq_message_destroy(inner);
  }) : undefined;
const lmq_message_ref_registry = typeof FinalizationRegistry !== "undefined" ? 
  new FinalizationRegistry((inner: Deno.PointerValue) => {
    ffi.lmq_message_peek_release(inner);
  }) : undefined;
const lmq_vec_registry = typeof FinalizationRegistry !== "undefined" ? 
  new FinalizationRegistry((inner: Deno.PointerValue) => {
    ffi.lmq_vec_destroy(inner);
  }) : undefined;

/**
 * Message.
 * 
 * Must be freed with the `free` method after use. Freeing the message will also invalidate any read views obtained. It will not invalidate claimed data.
 */
export class NextMessage {
  #_inner: Deno.PointerValue;
  #_ref: Deno.PointerValue;
  #_handle: Deno.PointerValue;
  #_current_view?: ArrayBuffer;

  private constructor(inner: Deno.PointerValue) {
    this.#_inner = inner;
    this.#_ref = null;
    this.#_handle = null;
    lmq_message_registry?.register(this, this.#_inner, this);
  }

  private _release_owned() {
    if (this.#_ref) {
      lmq_message_ref_registry?.unregister(this);
      ffi.lmq_message_peek_release(this.#_ref);
      this.#_ref = null;
    }
    if (this.#_handle) {
      lmq_vec_registry?.unregister(this);
      ffi.lmq_vec_destroy(this.#_handle);
      this.#_handle = null;
    }
  }

  private get _ref() {
    return this.#_ref;
  }

  private set _ref(pointer: Deno.PointerValue) {
    this._release_owned();
    this.#_ref = pointer;
    if (this.#_ref) {
      lmq_message_ref_registry?.register(this, this.#_ref, this);
    }
  }

  private get _handle() {
    return this.#_handle;
  }

  private set _handle(pointer: Deno.PointerValue) {
    this._release_owned();
    this.#_handle = pointer;
    if (this.#_handle) {
      lmq_vec_registry?.register(this, this.#_handle, this);
    }
  }

  static _createMessageFromRaw(inner: Deno.PointerValue): NextMessage {
    return new NextMessage(inner);
  }

  free(): void {
    this._release_owned();
    if (this.#_inner) {
      lmq_message_registry?.unregister(this);
      ffi.lmq_message_destroy(this.#_inner);
      this.#_inner = null;
    }
  }

  private _view(get: (message: Deno.PointerValue, ptr_buf: BufferSource | null, len_buf: BufferSource | null) => Deno.PointerValue): ArrayBuffer | undefined {
    const dataPtrBuf = new BigUint64Array(1);
    const dataLenBuf = new BigUint64Array(1);
    if (get(this.#_inner, dataPtrBuf, dataLenBuf)) {
      const dataLen = dataLenBuf[0];
      const dataView = new Deno.UnsafePointerView(
        Deno.UnsafePointer.create(
          dataPtrBuf[0],
        ) as Deno.PointerObject,
      ).getArrayBuffer(Number(dataLen));
      this.#_current_view = dataView;
      return dataView;
    } else {
      this.#_current_view = undefined;
      return undefined;
    }
  }

  /**
   * Read the message without claiming it. Zero-copy.
   * 
   * The view is guaranteed to remain valid as long as `NextMessage` is not dropped.
   * 
   * Returns `undefined` if the message has since been claimed.
   * 
   * @returns `Uint8Array` view of message data, or `undefined` if the message has been claimed already.
   */
  read(): Uint8Array | undefined {
    const arrayBuffer = this._view((message, ptr_buf, len_buf) => {
      this._ref = ffi.lmq_message_peek(message, ptr_buf, len_buf);
      return this._ref;
    });
    return arrayBuffer ? new Uint8Array(arrayBuffer) : undefined;
  }

  /**
   * Claim the message and return the payload.
   * 
   * Returns `undefined` if the message has since been claimed.
   * 
   * @returns `Uint8Array` view of message data, or `undefined` if the message has been claimed already.
   */
  claim(): Uint8Array | undefined {
    const arrayBuffer = this._view((message, ptr_buf, len_buf) => {
      this._handle = ffi.lmq_message_claim(message, ptr_buf, len_buf);
      return this._handle;
    });
    return arrayBuffer ? new Uint8Array(copyArrayBuffer(arrayBuffer)) : undefined;
  }

  /**
   * Get another reference to a previously created view of the underlying data.
   */
  get current_view() {
    return this.#_current_view;
  }
}

function copyArrayBuffer(src: ArrayBuffer)  {
  const dst = new ArrayBuffer(src.byteLength);
  new Uint8Array(dst).set(new Uint8Array(src));
  return dst;
}

