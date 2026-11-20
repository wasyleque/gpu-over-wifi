#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <sys/mman.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <linux/userfaultfd.h>
int main(int argc,char**argv){
  int flags = O_CLOEXEC|O_NONBLOCK|(argc>1?UFFD_USER_MODE_ONLY:0);
  int fd=syscall(SYS_userfaultfd,flags);
  if(fd<0){printf("userfaultfd(%s) failed: %s\n",argc>1?"USER_MODE_ONLY":"kernel-allowed",strerror(errno));return 1;}
  struct uffdio_api api={.api=UFFD_API,.features=UFFD_FEATURE_WP_UNPOPULATED};
  if(ioctl(fd,UFFDIO_API,&api)){perror("api");return 1;}
  size_t len=1<<20; char*p=mmap(0,len,PROT_READ|PROT_WRITE,MAP_SHARED|MAP_ANONYMOUS,-1,0);
  memset(p,1,len);
  struct uffdio_register r={.range={(unsigned long)p,len},.mode=UFFDIO_REGISTER_MODE_WP};
  if(ioctl(fd,UFFDIO_REGISTER,&r)){perror("register");return 1;}
  struct uffdio_writeprotect w={.range={(unsigned long)p,len},.mode=UFFDIO_WRITEPROTECT_MODE_WP};
  if(ioctl(fd,UFFDIO_WRITEPROTECT,&w)){perror("wp");return 1;}
  int f=open("/dev/zero",O_RDONLY);
  errno=0; ssize_t n=read(f,p+4096,4096);
  printf("mode=%s: read() do strony chronionej WP (sync) -> %zd errno=%d (%s)\n",argc>1?"USER_MODE_ONLY":"kernel-allowed",n,errno,strerror(errno));
  return 0;}
