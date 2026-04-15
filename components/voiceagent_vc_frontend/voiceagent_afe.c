#include "voiceagent_afe.h"

#include <string.h>
#include <esp_log.h>
#include <esp_heap_caps.h>
#include <freertos/FreeRTOS.h>
#include <freertos/task.h>
#include <freertos/queue.h>

#include "esp_afe_sr_iface.h"
#include "esp_afe_sr_models.h"
#include "esp_afe_config.h"

static const char *TAG = "voiceagent_afe";

/* ------------------------------------------------------------------ */
/*  Module state                                                       */
/* ------------------------------------------------------------------ */

static const esp_afe_sr_iface_t *s_afe_handle = NULL;
static esp_afe_sr_data_t        *s_afe_data   = NULL;
static int                       s_channels   = 0;
static int                       s_feed_chunk = 0;  /* per-call sample count */
static int                       s_fetch_chunk = 0;

/* Queue to pass processed frames from fetch-task → Rust caller */
#define FETCH_QUEUE_LEN  8
static QueueHandle_t s_fetch_queue = NULL;

/* Buffer handed out via queue (allocated once, reused) */
typedef struct {
    int16_t *data;
    int      size;      /* number of int16 samples */
} afe_frame_t;

static TaskHandle_t s_fetch_task = NULL;

/* ------------------------------------------------------------------ */
/*  Fetch task — runs on core 1                                        */
/* ------------------------------------------------------------------ */

static void afe_fetch_task(void *arg)
{
    ESP_LOGI(TAG, "AFE fetch task started (core %d), fetch_chunk=%d",
             xPortGetCoreID(), s_fetch_chunk);

    while (s_afe_data != NULL) {
        afe_fetch_result_t *res = s_afe_handle->fetch(s_afe_data);
        if (res == NULL || res->ret_value == ESP_FAIL) {
            vTaskDelay(pdMS_TO_TICKS(10));
            continue;
        }

        /* Copy into a heap buffer and push to queue */
        int n_samples = res->data_size / sizeof(int16_t);
        size_t bytes = n_samples * sizeof(int16_t);
        int16_t *buf = heap_caps_malloc(bytes, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
        if (buf == NULL) {
            buf = malloc(bytes);
        }
        if (buf == NULL) {
            ESP_LOGW(TAG, "fetch malloc failed (%d bytes)", (int)bytes);
            continue;
        }
        memcpy(buf, res->data, bytes);

        afe_frame_t frame = { .data = buf, .size = n_samples };
        if (xQueueSend(s_fetch_queue, &frame, pdMS_TO_TICKS(100)) != pdTRUE) {
            /* Queue full — drop oldest */
            afe_frame_t old;
            if (xQueueReceive(s_fetch_queue, &old, 0) == pdTRUE) {
                free(old.data);
            }
            xQueueSend(s_fetch_queue, &frame, 0);
        }
    }

    ESP_LOGI(TAG, "AFE fetch task exiting");
    vTaskDelete(NULL);
}

/* ------------------------------------------------------------------ */
/*  Public API                                                         */
/* ------------------------------------------------------------------ */

int voiceagent_afe_create(int mic_channels, bool has_reference)
{
    if (s_afe_data != NULL) {
        ESP_LOGW(TAG, "AFE already created, destroying first");
        voiceagent_afe_destroy();
    }

    /* Suppress the "Ringbuffer of AFE is empty" warning from ESP-SR internals.
     * With push-to-talk the mic is idle most of the time, so this is expected. */
    esp_log_level_set("AFE", ESP_LOG_ERROR);

    s_channels = mic_channels + (has_reference ? 1 : 0);
    int ref_num = has_reference ? 1 : 0;

    const char *fmt = has_reference ? "MR" : "M";
    afe_config_t *cfg = afe_config_init(fmt, NULL, AFE_TYPE_VC, AFE_MODE_LOW_COST);
    if (cfg == NULL) {
        ESP_LOGE(TAG, "afe_config_init failed");
        return -1;
    }

    /* AEC — only useful if we have a reference channel */
    cfg->aec_init = has_reference;

    /* SE (beamforming) — not needed for single mic */
    cfg->se_init = false;

    /* VAD — not needed for our pipeline (server does endpointing) */
    cfg->vad_init = false;

    /* WakeNet — not needed */
    cfg->wakenet_init = false;

    /* NS (Noise Suppression) — use WebRTC mode (no model file needed) */
    cfg->ns_init = true;
    cfg->afe_ns_mode = AFE_NS_MODE_WEBRTC;

    /* AGC — automatic gain control for voice communication */
    cfg->agc_init = true;
    cfg->agc_mode = AFE_AGC_MODE_WEBRTC;
    cfg->agc_compression_gain_db = 9;
    cfg->agc_target_level_dbfs = 3;

    /* AFE general */
    cfg->afe_perferred_core = 1;
    cfg->afe_perferred_priority = 5;
    cfg->afe_ringbuf_size = 50;
    cfg->memory_alloc_mode = AFE_MEMORY_ALLOC_MORE_PSRAM;
    cfg->afe_linear_gain = 1.0f;
    cfg->fixed_first_channel = true;

    /* Debug off */
    cfg->debug_init = false;

    /* Validate the config */
    cfg = afe_config_check(cfg);
    if (cfg == NULL) {
        ESP_LOGE(TAG, "afe_config_check failed");
        return -2;
    }

    afe_config_print(cfg);

    /* Create the AFE handle from config */
    s_afe_handle = esp_afe_handle_from_config(cfg);
    if (s_afe_handle == NULL) {
        ESP_LOGE(TAG, "esp_afe_handle_from_config returned NULL");
        afe_config_free(cfg);
        return -3;
    }

    /* Create AFE data (the actual processing pipeline) */
    s_afe_data = s_afe_handle->create_from_config(cfg);
    if (s_afe_data == NULL) {
        ESP_LOGE(TAG, "create_from_config failed");
        afe_config_free(cfg);
        s_afe_handle = NULL;
        return -4;
    }

    s_feed_chunk = s_afe_handle->get_feed_chunksize(s_afe_data);
    s_fetch_chunk = s_afe_handle->get_fetch_chunksize(s_afe_data);

    ESP_LOGI(TAG, "AFE created: channels=%d, feed_chunk=%d samples, fetch_chunk=%d samples",
             s_channels, s_feed_chunk, s_fetch_chunk);

    /* Print the processing pipeline */
    s_afe_handle->print_pipeline(s_afe_data);

    afe_config_free(cfg);

    /* Create fetch queue + task */
    s_fetch_queue = xQueueCreate(FETCH_QUEUE_LEN, sizeof(afe_frame_t));
    xTaskCreatePinnedToCore(afe_fetch_task, "afe_fetch", 4096 * 2, NULL,
                            5, &s_fetch_task, 1);

    return 0;
}

int voiceagent_afe_get_feed_chunksize(void)
{
    return s_feed_chunk * s_channels;
}

int voiceagent_afe_get_fetch_chunksize(void)
{
    return s_fetch_chunk;
}

int voiceagent_afe_feed(const int16_t *samples, int count)
{
    if (s_afe_data == NULL || s_afe_handle == NULL) {
        return -1;
    }

    int chunk = s_feed_chunk * s_channels;
    int consumed = 0;

    while (consumed + chunk <= count) {
        s_afe_handle->feed(s_afe_data, samples + consumed);
        consumed += chunk;
    }

    return consumed;
}

int voiceagent_afe_fetch(int16_t *out, int *out_size)
{
    if (s_fetch_queue == NULL || out == NULL || out_size == NULL) {
        return -1;
    }

    afe_frame_t frame;
    if (xQueueReceive(s_fetch_queue, &frame, pdMS_TO_TICKS(200)) != pdTRUE) {
        *out_size = 0;
        return -1;  /* timeout */
    }

    int copy = frame.size;
    if (copy > *out_size) {
        copy = *out_size;
    }
    memcpy(out, frame.data, copy * sizeof(int16_t));
    *out_size = copy;
    free(frame.data);

    return 0;
}

int voiceagent_afe_fetch_nonblocking(int16_t *out, int *out_size)
{
    if (s_fetch_queue == NULL || out == NULL || out_size == NULL) {
        return -1;
    }

    afe_frame_t frame;
    if (xQueueReceive(s_fetch_queue, &frame, 0) != pdTRUE) {
        *out_size = 0;
        return -1;  /* nothing available */
    }

    int copy = frame.size;
    if (copy > *out_size) {
        copy = *out_size;
    }
    memcpy(out, frame.data, copy * sizeof(int16_t));
    *out_size = copy;
    free(frame.data);

    return 0;
}

void voiceagent_afe_destroy(void)
{
    if (s_afe_data != NULL && s_afe_handle != NULL) {
        /* Signal fetch task to stop */
        esp_afe_sr_data_t *tmp = s_afe_data;
        s_afe_data = NULL;
        vTaskDelay(pdMS_TO_TICKS(300)); /* let fetch task exit */

        s_afe_handle->destroy(tmp);
        s_afe_handle = NULL;
    }

    /* Drain and delete queue */
    if (s_fetch_queue != NULL) {
        afe_frame_t frame;
        while (xQueueReceive(s_fetch_queue, &frame, 0) == pdTRUE) {
            free(frame.data);
        }
        vQueueDelete(s_fetch_queue);
        s_fetch_queue = NULL;
    }

    s_fetch_task = NULL;
    s_feed_chunk = 0;
    s_fetch_chunk = 0;
    s_channels = 0;

    ESP_LOGI(TAG, "AFE destroyed");
}
