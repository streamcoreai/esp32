/**
 * @file voiceagent_afe.h
 * @brief Thin C wrapper around Espressif's ESP-SR AFE (Audio Front End)
 *        for voice-communication mode.  Provides AGC + Noise Suppression
 *        on the mic signal so the Opus encoder sends clean audio to the server.
 *
 * Usage from Rust (via FFI):
 *   1. voiceagent_afe_create(1, false)   -- 1 mic channel, no reference
 *   2. loop:
 *        voiceagent_afe_feed(pcm16, feed_size)
 *        voiceagent_afe_fetch(out, &out_size)   -- blocks until ready
 *   3. voiceagent_afe_destroy()
 */

#pragma once

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Create and initialise the AFE voice-communication pipeline.
 *
 * @param mic_channels   Number of microphone channels (typically 1).
 * @param has_reference  true if the audio stream contains a speaker-reference
 *                       channel (for AEC).  false for simplex setups.
 * @param enable_wakenet true to also run WakeNet inside AFE — when set, each
 *                       fetched frame may carry a wake-up event which the
 *                       caller can poll via voiceagent_afe_consume_wake().
 *                       The wake-word model used is the one selected in
 *                       sdkconfig (`CONFIG_SR_WN9_*`).
 * @return 0 on success, negative on error.
 */
int voiceagent_afe_create(int mic_channels, bool has_reference, bool enable_wakenet);

/**
 * @brief Return the number of int16 samples expected by each feed() call.
 *        This includes all channels (mic + ref if any).
 */
int voiceagent_afe_get_feed_chunksize(void);

/**
 * @brief Return the number of int16 samples produced by each fetch() call.
 *        This is always single-channel.
 */
int voiceagent_afe_get_fetch_chunksize(void);

/**
 * @brief Feed raw 16-bit PCM samples into the AFE pipeline.
 *
 * @param samples  Pointer to interleaved int16 samples.
 * @param count    Number of int16 samples (must be a multiple of feed_chunksize).
 * @return Number of samples consumed, or negative on error.
 */
int voiceagent_afe_feed(const int16_t *samples, int count);

/**
 * @brief Fetch one frame of processed (AGC + NS) audio from the AFE.
 *        This call **blocks** until a frame is ready (up to ~200ms).
 *
 * @param out       Caller-allocated buffer for output samples.
 * @param out_size  [in] capacity in int16 samples.  [out] samples written.
 * @return 0 on success, -1 on timeout / error.
 */
int voiceagent_afe_fetch(int16_t *out, int *out_size);

/**
 * @brief Non-blocking fetch — returns immediately if no frame is ready.
 *
 * @param out       Caller-allocated buffer for output samples.
 * @param out_size  [in] capacity in int16 samples.  [out] samples written.
 * @return 0 on success, -1 if no frame available.
 */
int voiceagent_afe_fetch_nonblocking(int16_t *out, int *out_size);

/**
 * @brief Returns 1 if a wake-word event was detected since the last call to
 *        this function, 0 otherwise. The flag latches on detection and is
 *        cleared by reading it. Only meaningful when WakeNet was enabled at
 *        create() time.
 */
int voiceagent_afe_consume_wake(void);

/**
 * @brief Tear down the AFE pipeline and free all resources.
 */
void voiceagent_afe_destroy(void);

#ifdef __cplusplus
}
#endif
