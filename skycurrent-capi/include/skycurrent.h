#ifndef SKYCURRENT_H
#define SKYCURRENT_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Forward declaration of an LMQ message consumer.
 */
typedef struct lmq_consumer_t lmq_consumer_t;

/**
 * Should-collect callback function type.
 * 
 * Callback should return a `bool` indicating if the message with the given header data should be collected.
 */
typedef bool (*skycurrent_should_collect_callback_t)(const uint8_t *p, size_t len, void *user_data);

/**
 * Sets the global project directory, required for certain backings.
 * 
 * Returns 0 if successful, and 1 if the provided path was not a valid UTF-8 string.
 */
int sc_set_global_project_dir(const char *project_dir);

/**
 * Sets the global `should_collect` callback, required for certain backings.
 * 
 * Given callback must be thread-safe. A single callback might be called simultaneously from multiple threads.
 * 
 * `user_data` is passed *as-is* to the callback; it is the user's responsibility to make sure that the `user_data` pointer is thread-safe.
 */
void sc_set_global_should_collect(skycurrent_should_collect_callback_t should_collect,
                                  void *user_data);

/**
 * Initialize SkyCurrent.
 */
void sc_init(void);

/**
 * Send an arbitrarily-sized payload.
 * 
 * Since this might require reassembly of data on the receiver-side on certain backings, all payloads should have a small header section so that receivers can decide if they want to reconstruct the message or pass on it, saving memory and execution time.
 * 
 * The `header_size` specifies how many bytes from the head of the payload corresponds to the header; every fragment sent will include the header but have different sections of the body.
 */
void sc_send_stream(const uint8_t *data, size_t len, size_t header_size);

/**
 * Get an iterator over the message stream.
 * 
 * This returns an [`lmq_consumer_t`] that can be used to iterate through and process unclaimed incoming messages.
 * 
 * The returned consumer must be freed using `lmq_consumer_destroy` after usage.
 */
lmq_consumer_t *sc_iter_stream(void);

/**
 * Close SkyCurrent.
 */
void sc_close(void);

#ifdef __cplusplus
}
#endif

#endif

