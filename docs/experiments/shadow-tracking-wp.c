// Eksperyment: koszt sledzenia zmienionych stron pamieci wspoldzielonej (memfd).
// A: mprotect + SIGSEGV   B: userfaultfd WP_ASYNC + PAGEMAP_SCAN (bez sygnalow)
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <signal.h>
#include <errno.h>
#include <fcntl.h>
#include <time.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <linux/userfaultfd.h>
#include <linux/fs.h>

#define PG 4096UL
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec*1e3+t.tv_nsec/1e6;}

static uint8_t *A; static size_t A_len; static volatile uint8_t *dirty;
static void on_segv(int sig, siginfo_t *si, void *u){
    (void)sig;(void)u;
    uintptr_t a=(uintptr_t)si->si_addr & ~(PG-1);
    if(a>=(uintptr_t)A && a<(uintptr_t)A+A_len){
        dirty[(a-(uintptr_t)A)/PG]=1;
        mprotect((void*)a,PG,PROT_READ|PROT_WRITE);
    } else _exit(3);
}

int main(int argc,char**argv){
    size_t mb = argc>1? atoi(argv[1]):100;
    size_t len = mb<<20, pages=len/PG; A_len=len;
    int fd=memfd_create("shadow",0); ftruncate(fd,len);
    dirty=calloc(pages,1);

    // --- A: mprotect + SIGSEGV
    A=mmap(0,len,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
    memset(A,1,len); // wstepne zapelnienie (strony obecne)
    struct sigaction sa={0}; sa.sa_sigaction=on_segv; sa.sa_flags=SA_SIGINFO; sigaction(SIGSEGV,&sa,0);
    double t0=now(); mprotect(A,len,PROT_READ); double t1=now();
    for(size_t i=0;i<pages;i++) A[i*PG]=2;
    double t2=now();
    size_t nd=0; for(size_t i=0;i<pages;i++) nd+=dirty[i];
    printf("A mprotect+SIGSEGV  %zu MiB (%zu stron): zalozenie ochrony %.2f ms, zapisy %.2f ms => %.2f us/strone, wykryto %zu\n",
           mb,pages,t1-t0,t2-t1,(t2-t1)*1000.0/pages,nd);
    munmap(A,len);

    // --- B: userfaultfd WP_ASYNC + PAGEMAP_SCAN
    uint8_t *B=mmap(0,len,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
    int uffd=syscall(__NR_userfaultfd,O_CLOEXEC|O_NONBLOCK|UFFD_USER_MODE_ONLY);
    if(uffd<0){printf("B userfaultfd(UFFD_USER_MODE_ONLY) nieudane: %s\n",strerror(errno));return 0;}
    struct uffdio_api api={.api=UFFD_API,.features=UFFD_FEATURE_WP_ASYNC|UFFD_FEATURE_WP_UNPOPULATED};
    if(ioctl(uffd,UFFDIO_API,&api)<0){printf("B UFFDIO_API (WP_ASYNC) nieudane: %s\n",strerror(errno));return 0;}
    memset(B,1,len);
    struct uffdio_register reg={.range={.start=(uintptr_t)B,.len=len},.mode=UFFDIO_REGISTER_MODE_WP};
    if(ioctl(uffd,UFFDIO_REGISTER,&reg)<0){printf("B UFFDIO_REGISTER nieudane: %s\n",strerror(errno));return 0;}
    struct uffdio_writeprotect wp={.range={.start=(uintptr_t)B,.len=len},.mode=UFFDIO_WRITEPROTECT_MODE_WP};
    double u0=now(); if(ioctl(uffd,UFFDIO_WRITEPROTECT,&wp)<0){printf("B WRITEPROTECT nieudane: %s\n",strerror(errno));return 0;} double u1=now();
    for(size_t i=0;i<pages;i++) B[i*PG]=2;
    double u2=now();
    int pm=open("/proc/self/pagemap",O_RDONLY);
    static struct page_region vec[4096];
    struct pm_scan_arg arg={.size=sizeof(arg),.flags=PM_SCAN_WP_MATCHING|PM_SCAN_CHECK_WPASYNC,
        .start=(uintptr_t)B,.end=(uintptr_t)B+len,.vec=(uintptr_t)vec,.vec_len=4096,
        .category_mask=PAGE_IS_WRITTEN,.return_mask=PAGE_IS_WRITTEN};
    long r=ioctl(pm,PAGEMAP_SCAN,&arg); double u3=now();
    if(r<0){printf("B PAGEMAP_SCAN nieudane: %s\n",strerror(errno));return 0;}
    size_t dp=0; for(long k=0;k<r;k++) dp+=(vec[k].end-vec[k].start)/PG;
    printf("B uffd WP_ASYNC     %zu MiB (%zu stron): zalozenie ochrony %.2f ms, zapisy %.2f ms => %.2f us/strone, skan %.2f ms (%ld zakresow, wykryto %zu stron)\n",
           mb,pages,u1-u0,u2-u1,(u2-u1)*1000.0/pages,u3-u2,r,dp);
    // kolejna runda: bez zapisow nic nie powinno byc zmienione (ochrona zalozona ponownie przez skan)
    arg.start=(uintptr_t)B; r=ioctl(pm,PAGEMAP_SCAN,&arg);
    printf("B po skanie, bez zapisow: zakresow %ld (oczekiwano 0)\n",r);
    return 0;
}
