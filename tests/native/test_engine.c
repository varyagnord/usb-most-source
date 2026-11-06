#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <poll.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <linux/usbdevice_fs.h>
#include <sys/ioctl.h>
static struct usbdevfs_urb *submitted[128];
static int count, submit_calls, discard_calls, closed, submit_error, cancel_error;
static struct usbdevfs_urb *completed;
static int mock_dup(int fd) { assert(fd==42);return 43; }
static int mock_close(int fd) { assert(fd==43);closed++;return 0; }
static int mock_poll(struct pollfd *fds,nfds_t n,int timeout) {assert(n==1&&timeout==100);fds->revents=0;return 0;}
static int mock_ioctl(int fd,unsigned long code,void *arg) {
    assert(fd==43);
    if(code==USBDEVFS_SUBMITURB) {
        submit_calls++;
        if(submit_error){errno=submit_error;return -1;}
        submitted[count++]=arg;return 0;
    }
    if(code==USBDEVFS_DISCARDURB){discard_calls++;if(cancel_error){errno=cancel_error;return -1;}return 0;}
    assert(code==USBDEVFS_REAPURBNDELAY);
    if(completed){*(struct usbdevfs_urb **)arg=completed;completed=NULL;return 0;}
    errno=EAGAIN;return -1;
}
#define ENGINE_DUP mock_dup
#define ENGINE_CLOSE mock_close
#define ENGINE_IOCTL mock_ioctl
#define ENGINE_POLL mock_poll
#include "../reference/urb_engine.c"

static void finish(struct usbdevfs_urb *urb,int status,int actual){urb->status=status;urb->actual_length=actual;completed=urb;}
int main(void){
    urb_engine *e=engine_open(42);assert(e);
    urb_result result;
    int sizes[]={0,1,16,63,64,65,127,128,129,130,256,270,271,512,65536,65556,131072};
    for(unsigned i=0;i<sizeof(sizes)/sizeof(sizes[0]);i++){
        int size=sizes[i],capacity=size<271?271:size;
        assert(engine_submit(e,(int)i,USBDEVFS_URB_TYPE_BULK,0x82,0,NULL,capacity,NULL)==0);
        struct usbdevfs_urb *urb=submitted[count-1];memset(urb->buffer,0x5a,size);
        finish(urb,0,size);assert(engine_reap(e,&result,100)==1);
        assert(result.actual==size&&result.status==0&&result.id==(int)i);
        for(int n=0;n<size;n++)assert(result.data[n]==0x5a);
        engine_result_free(&result);
    }
    unsigned char data[130];memset(data,0xa5,sizeof(data));
    int before=submit_calls;
    assert(engine_submit(e,100,3,2,0,data,130,NULL)==0);
    assert(memcmp(submitted[count-1]->buffer,data,130)==0);
    finish(submitted[count-1],-ETIMEDOUT,64);assert(engine_reap(e,&result,100)==1);
    assert(result.actual==64&&result.status==-ETIMEDOUT&&result.data==NULL);
    assert(submit_calls==before+1); // A timed-out OUT is never retried.
    assert(engine_submit(e,101,3,130,0,NULL,271,NULL)==0);
    memset(submitted[count-1]->buffer,7,130);finish(submitted[count-1],-ETIMEDOUT,130);
    assert(engine_reap(e,&result,100)==1&&result.actual==130&&result.data[129]==7);engine_result_free(&result);
    unsigned char setup[]={0x80,6,0,1,0,0,18,0};
    assert(engine_submit(e,102,2,128,0,NULL,18,setup)==0);
    struct usbdevfs_urb *control=submitted[count-1];
    assert(control->buffer_length==26&&memcmp(control->buffer,setup,8)==0);
    memset((unsigned char *)control->buffer+8,0xee,18);finish(control,0,18);
    assert(engine_reap(e,&result,100)==1&&result.actual==18&&result.data[0]==0xee);engine_result_free(&result);
    assert(engine_submit(e,103,3,130,0,NULL,271,NULL)==0);
    assert(engine_submit(e,103,3,130,0,NULL,271,NULL)==-EEXIST);
    assert(engine_cancel(e,103)==0&&discard_calls==1);
    finish(submitted[count-1],-ECONNRESET,0);assert(engine_reap(e,&result,100)==1&&result.status==-ECONNRESET);
    assert(engine_cancel(e,103)==-ENOENT);
    assert(engine_submit(e,104,3,130,0,NULL,1,NULL)==0);
    finish(submitted[count-1],0,2);assert(engine_reap(e,&result,100)==1&&result.status==-EOVERFLOW&&result.actual==0);
    submit_error=ENODEV;assert(engine_submit(e,105,3,130,0,NULL,1,NULL)==-ENODEV);submit_error=0;
    assert(engine_reap(e,&result,100)==0);
    assert(engine_submit(e,106,3,2,0,NULL,1,NULL)==-EINVAL);
    assert(engine_submit(e,106,3,130,0,NULL,131073,NULL)==-EINVAL);
    assert(engine_submit(e,106,2,128,0,NULL,65536,setup)==-EINVAL);
    assert(engine_submit(e,107,3,130,0x40200,NULL,65556,NULL)==0);
    memset(submitted[count-1]->buffer,0x3c,130);finish(submitted[count-1],0,130);
    assert(engine_reap(e,&result,100)==1&&result.actual==130&&result.data[129]==0x3c);
    engine_result_free(&result);
    assert(engine_submit(e,106,0,130,0,NULL,1,NULL)==-EINVAL);
    for(int i=0;i<64;i++)assert(engine_submit(e,1000+i,1,129,0,NULL,16,NULL)==0);
    assert(engine_submit(e,2000,1,129,0,NULL,16,NULL)==-ENOSPC);
    // Closing a session disposes every outstanding native buffer, including interrupt reads.
    engine_close(e);assert(closed==1);
    puts("PASS native USB engine: exact lengths, partial errors, cancellation, control setup, queue bounds, cleanup");
    return 0;
}
