#define _GNU_SOURCE
#include "urb_engine.h"
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <poll.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>
#ifndef ENGINE_IOCTL
#define ENGINE_IOCTL ioctl
#endif
#ifndef ENGINE_DUP
#define ENGINE_DUP(fd) fcntl(fd, F_DUPFD_CLOEXEC, 0)
#endif
#ifndef ENGINE_CLOSE
#define ENGINE_CLOSE close
#endif
#ifndef ENGINE_POLL
#define ENGINE_POLL poll
#endif
typedef struct request {
    int32_t id;
    int capacity, input, offset;
    struct request *next;
    struct usbdevfs_urb urb; /* Flexible ISO array remains last (ISO is unsupported). */
} request;
struct urb_engine { int fd; unsigned count; pthread_mutex_t lock; request *head; };
urb_engine *engine_open(int fd) {
    urb_engine *e = calloc(1, sizeof(*e));
    if (!e) return NULL;
    e->fd = ENGINE_DUP(fd);
    if (e->fd < 0) { free(e); return NULL; }
    int err = pthread_mutex_init(&e->lock, NULL);
    if (err) { ENGINE_CLOSE(e->fd); free(e); errno = err; return NULL; }
    return e;
}
static void free_request(request *r) {
    if (r->urb.buffer) {
        volatile unsigned char *p = r->urb.buffer;
        for (int i = 0; i < r->urb.buffer_length; i++) p[i] = 0;
        free(r->urb.buffer);
    }
    free(r);
}
int engine_submit(urb_engine *e, int32_t id, unsigned type, unsigned endpoint,
                  unsigned flags, const unsigned char *data, int length, const unsigned char *setup) {
    if (!e || length < 0 || length > BRIDGE_MAX_TRANSFER_BYTES || endpoint > 255 ||
        (type != USBDEVFS_URB_TYPE_BULK && type != USBDEVFS_URB_TYPE_INTERRUPT &&
         type != USBDEVFS_URB_TYPE_CONTROL) ||
        (type == USBDEVFS_URB_TYPE_CONTROL && (!setup || length > 65535))) return -EINVAL;
    request *r = calloc(1, sizeof(*r));
    if (!r) return -ENOMEM;
    r->id = id; r->capacity = length;
    r->offset = type == USBDEVFS_URB_TYPE_CONTROL ? 8 : 0;
    r->input = type == USBDEVFS_URB_TYPE_CONTROL ? (setup[0] & 0x80) : (endpoint & 0x80);
    r->urb.type = type; r->urb.endpoint = endpoint;
    r->urb.flags = flags & (USBDEVFS_URB_SHORT_NOT_OK | USBDEVFS_URB_ZERO_PACKET);
    r->urb.buffer_length = length + r->offset;
    r->urb.buffer = calloc(1, r->urb.buffer_length ? (size_t)r->urb.buffer_length : 1);
    if (!r->urb.buffer) { free(r); return -ENOMEM; }
    if (r->offset) memcpy(r->urb.buffer, setup, 8);
    if (length && !r->input) {
        if (!data) { free_request(r); return -EINVAL; }
        memcpy((unsigned char *)r->urb.buffer + r->offset, data, length);
    }
    r->urb.usercontext = r;
    pthread_mutex_lock(&e->lock);
    int result = 0;
    if (e->count >= 64) result = -ENOSPC;
    for (request *p = e->head; p && !result; p = p->next) if (p->id == id) result = -EEXIST;
    if (!result && ENGINE_IOCTL(e->fd, USBDEVFS_SUBMITURB, &r->urb) < 0) result = -errno;
    if (!result) { r->next = e->head; e->head = r; e->count++; }
    pthread_mutex_unlock(&e->lock);
    if (result) free_request(r);
    return result;
}
int engine_cancel(urb_engine *e, int32_t id) {
    pthread_mutex_lock(&e->lock);
    int result = -ENOENT;
    for (request *r = e->head; r; r = r->next) {
        if (r->id != id) continue;
        result = ENGINE_IOCTL(e->fd, USBDEVFS_DISCARDURB, &r->urb) < 0 ? -errno : 0;
        break;
    }
    pthread_mutex_unlock(&e->lock);
    return result;
}
int engine_reap(urb_engine *e, urb_result *out, int timeout_ms) {
    memset(out, 0, sizeof(*out));
    pthread_mutex_lock(&e->lock);
    struct usbdevfs_urb *urb = NULL;
    int rc = ENGINE_IOCTL(e->fd, USBDEVFS_REAPURBNDELAY, &urb);
    int error = rc < 0 ? errno : 0;
    if (rc == 0) {
        request **slot = &e->head;
        while (*slot && &(*slot)->urb != urb) slot = &(*slot)->next;
        if (!*slot) { pthread_mutex_unlock(&e->lock); return -EPROTO; }
        request *r = *slot; *slot = r->next; e->count--;
        out->id = r->id; out->status = urb->status; out->actual = urb->actual_length;
        if (out->actual < 0 || out->actual > r->capacity) { out->status = -EOVERFLOW; out->actual = 0; }
        if (r->input && out->actual) {
            out->data = malloc((size_t)out->actual);
            if (!out->data) { out->status = -ENOMEM; out->actual = 0; }
            else memcpy(out->data, (unsigned char *)urb->buffer + r->offset, out->actual);
        }
        free_request(r);
        pthread_mutex_unlock(&e->lock);
        return 1;
    }
    pthread_mutex_unlock(&e->lock);
    if (error != EAGAIN && error != EINTR) return -error;
    struct pollfd pfd = { .fd = e->fd, .events = POLLOUT };
    rc = ENGINE_POLL(&pfd, 1, timeout_ms);
    if (rc < 0 && errno != EINTR) return -errno;
    if (rc > 0 && (pfd.revents & (POLLERR | POLLHUP | POLLNVAL))) return -ENODEV;
    return 0;
}
void engine_result_free(urb_result *r) {
    if (r->data) {
        volatile unsigned char *p = r->data;
        for (int i = 0; i < r->actual; i++) p[i] = 0;
        free(r->data); r->data = NULL;
    }
}
void engine_close(urb_engine *e) {
    if (!e) return;
    /* Last fd close kills kernel URBs before buffers are freed. */
    ENGINE_CLOSE(e->fd);
    request *r = e->head;
    while (r) { request *next = r->next; free_request(r); r = next; }
    pthread_mutex_destroy(&e->lock); free(e);
}
