/* File-replay stand-in for librtlsdr, used only to benchmark vortrack as a black box.
   Reads cu8 IQ from $NAV_IQ and hands it to the async callback exactly like a dongle. */
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
typedef struct rtlsdr_dev rtlsdr_dev_t; struct rtlsdr_dev { int x; };
typedef void (*rtlsdr_read_async_cb_t)(unsigned char *buf, uint32_t len, void *ctx);
static struct rtlsdr_dev D;
uint32_t rtlsdr_get_device_count(void){return 1;}
const char* rtlsdr_get_device_name(uint32_t i){return "file";}
int rtlsdr_open(rtlsdr_dev_t **d, uint32_t i){*d=&D;return 0;}
int rtlsdr_get_tuner_gains(rtlsdr_dev_t *d,int *g){if(g)g[0]=400;return 1;}
int rtlsdr_set_tuner_gain_mode(rtlsdr_dev_t *d,int m){return 0;}
int rtlsdr_set_tuner_gain(rtlsdr_dev_t *d,int g){return 0;}
int rtlsdr_set_freq_correction(rtlsdr_dev_t *d,int p){return 0;}
int rtlsdr_set_center_freq(rtlsdr_dev_t *d,uint32_t f){return 0;}
int rtlsdr_set_sample_rate(rtlsdr_dev_t *d,uint32_t r){return 0;}
int rtlsdr_reset_buffer(rtlsdr_dev_t *d){return 0;}
int rtlsdr_read_async(rtlsdr_dev_t *d, rtlsdr_read_async_cb_t cb, void *ctx, uint32_t n, uint32_t len){
  FILE *f=fopen(getenv("NAV_IQ"),"rb"); if(!f){perror("NAV_IQ");return -1;}
  unsigned char *b=malloc(len); size_t r;
  while((r=fread(b,1,len,f))>0) cb(b,(uint32_t)(r&~1u),ctx);
  fclose(f); fflush(stdout); return 0;}
