#ifndef _LMQ_H
#define _LMQ_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum lmq_action_t {
  LMQ_ACTION_CONTINUE = 0,
  LMQ_ACTION_DEREGISTER = -1,
} lmq_action_t;

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
typedef lmq_action_t (*lmq_msg_callback_t)(struct lmq_message_t *message, void *user_data);

/**
 * Create a new linked message queue.
 * 
 * The returned queue must be freed with `lmq_destroy` after usage.
 */
struct lmq_t *lmq_new(void);

/**
 * Destroy queue.
 */
void lmq_destroy(struct lmq_t *queue);

/**
 * Push a message into the end of the queue.
 */
void lmq_push(struct lmq_t *queue, const uint8_t *data, size_t len);

/**
 * Create a new message consumer which consumes from the provided queue.
 * 
 * The returned consumer must be freed with `lmq_consumer_destroy` after usage.
 */
struct lmq_consumer_t *lmq_consumer_new(const struct lmq_t *queue);

/**
 * Destroy consumer.
 */
void lmq_consumer_destroy(struct lmq_consumer_t *consumer);

/**
 * Attempt to return the next unclaimed message without blocking.
 * 
 * Returns `NULL` if no new unclaimed messages are available.
 * 
 * The returned message must be freed with `lmq_message_destroy` after usage.
 */
struct lmq_message_t *lmq_consumer_try_next(struct lmq_consumer_t *consumer);

/**
 * Block until the next unclaimed message arrives.
 * 
 * The returned message must be freed with `lmq_message_destroy` after usage.
 */
struct lmq_message_t *lmq_consumer_wait(struct lmq_consumer_t *consumer);

/**
 * Register an `lmq_msg_callback_t` handler function that will be called whenever a new unclaimed message is available.
 * 
 * If the given callback had already been registered with the given queue previously, the old callback will be replaced with the new one. The old callback will soon stop being called.
 * 
 * It is the user's responsibility to destroy the provided message at some point in the future using `lmq_message_destroy` after usage.
 * 
 * Callback should return an `lmq_action_t` which can be used by the callback to immediately deregister itself.
 * 
 * Given callback must be thread-safe as internally handlers run on a separate thread controlled by the tokio runtime. A single callback might be called simultaneously from multiple threads.
 * 
 * `user_data` is passed *as-is* to the callback; it is the user's responsibility to make sure that the `user_data` pointer is thread-safe.
 * 
 * Only available when compiling with feature "tokio".
 */
void lmq_register_handler(const struct lmq_t *queue, lmq_msg_callback_t callback, void *user_data);

/**
 * Deregister a handler function.
 * 
 * Handlers must be deregistered from the same thread where it was originally registered.
 * 
 * Returns `TRUE` if a handler was successfully deregistered. If `FALSE` is returned, the provided handler was not registered with the given queue.
 */
bool lmq_deregister_handler(const struct lmq_t *queue, lmq_msg_callback_t callback);

/**
 * Destroy message.
 */
void lmq_message_destroy(struct lmq_message_t *message);

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
struct lmq_message_ref_t *lmq_message_peek(const struct lmq_message_t *message,
                                           const uint8_t **p,
                                           size_t *len);

/**
 * Destroy message read lock.
 */
void lmq_message_peek_release(struct lmq_message_ref_t *message_ref);

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
struct lmq_vec_t *lmq_message_claim(struct lmq_message_t *message, uint8_t **p, size_t *len);

/**
 * Destroy vector.
 */
void lmq_vec_destroy(struct lmq_vec_t *data);

#ifdef __cplusplus
}
#endif

#endif

