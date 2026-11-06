#ifndef RUTOKEN_URB_ENGINE_H
#define RUTOKEN_URB_ENGINE_H
#include <stdint.h>
#include <stddef.h>
#include <linux/usbdevice_fs.h>
/* Keep aligned with Java TransferLimits; libccid reads 65556 bytes at once. */
#define BRIDGE_MAX_TRANSFER_BYTES (128 * 1024)
/* Caller stops reaper and closes Android fd before engine_close. */
typedef struct urb_engine urb_engine;
typedef struct { int32_t id, status, actual; unsigned char *data; } urb_result;
urb_engine *engine_open(int fd);
int engine_submit(urb_engine *, int32_t id, unsigned type, unsigned endpoint,
                  unsigned flags, const unsigned char *data, int length, const unsigned char *setup);
int engine_cancel(urb_engine *, int32_t id);
int engine_reap(urb_engine *, urb_result *, int timeout_ms);
void engine_close(urb_engine *);
void engine_result_free(urb_result *);
#endif
