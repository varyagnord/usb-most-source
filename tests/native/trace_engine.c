#define main legacy_reference_main
#include "test_engine.c"
#undef main
int main(void) {
    urb_engine *e=engine_open(42);
    unsigned char data[131072];
    for(int i=0;i<96;i++) {
        int size=(i==0?65556:i==1?131072:(i*97)%1024);
        int input=i%2, type=i>1&&i%3==0?2:3;
        unsigned char setup[8]={(unsigned char)(input?0x80:0),6,0,1,0,0,0,0};
        memset(data,i,size);
        assert(engine_submit(e,i,type,input?130:2,0x40041,data,size,type==2?setup:NULL)==0);
        int actual=size?size-(i%(size+1)):0;
        if(input&&actual)memset((unsigned char *)submitted[count-1]->buffer+(type==2?8:0),i,actual);
        int status=i%4==0?-ETIMEDOUT:0;
        finish(submitted[count-1],status,actual);
        urb_result r;assert(engine_reap(e,&r,100)==1);
        unsigned checksum=0;if(r.data)for(int j=0;j<r.actual;j++)checksum+=r.data[j];
        printf("TRACE %d %d %d %u\n",r.id,r.status,r.actual,checksum);
        engine_result_free(&r);
    }
    engine_close(e);return 0;
}
