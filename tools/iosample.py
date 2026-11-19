import os,sys,time
tag=sys.argv[1]; out=open(os.path.expanduser(f"~/gpu-hub-client/io-{tag}.csv"),"w")
def game():
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                if open(f"/proc/{p}/comm").read().strip()=="ROTTR.exe": return True
            except: pass
    return False
def psi(f):
    try: return int(open(f"/proc/pressure/{f}").read().split("\n")[0].split("total=")[1])
    except: return 0
def st():
    a=open("/sys/block/nvme0n1/stat").read().split()
    return int(a[2]),int(a[6]),psi("io"),psi("memory"),int(a[3])+int(a[7]) # sektory odczyt, zapis, io µs, mem µs, ms io
while not game(): time.sleep(1)
out.write("t,read_MB,write_MB,io_stall_ms,mem_stall_ms\n"); p=st(); t0=time.time()
while game():
    time.sleep(1); c=st()
    out.write("%d,%.1f,%.1f,%d,%d\n"%(time.time()-t0,(c[0]-p[0])*512/1e6,(c[1]-p[1])*512/1e6,(c[2]-p[2])/1000,(c[3]-p[3])/1000)); out.flush(); p=c
