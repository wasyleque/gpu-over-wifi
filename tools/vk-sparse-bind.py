#!/usr/bin/env python3
# Test vkQueueBindSparse przez nasz ICD: bufor SPARSE_BINDING, alokacja pamięci, bindowanie, czekanie na kolejce.
import ctypes as C
vk = C.CDLL("libvulkan.so.1")
u32,u64,vp,i32 = C.c_uint32,C.c_uint64,C.c_void_p,C.c_int32
class S(C.Structure): pass
def st(name,fields): return type(name,(C.Structure,),{"_fields_":fields})
App=st("App",[("sType",u32),("pNext",vp),("n",C.c_char_p),("av",u32),("e",C.c_char_p),("ev",u32),("api",u32)])
Inst=st("Inst",[("sType",u32),("pNext",vp),("flags",u32),("app",C.POINTER(App)),("lc",u32),("l",vp),("ec",u32),("e",vp)])
app=App(0,None,b"sp",1,None,0,(1<<22)|(3<<12)); inst=vp()
print("instancja", vk.vkCreateInstance(C.byref(Inst(1,None,0,C.pointer(app),0,None,0,None)),None,C.byref(inst)))
n=u32(0); vk.vkEnumeratePhysicalDevices(inst,C.byref(n),None); pds=(vp*n.value)(); vk.vkEnumeratePhysicalDevices(inst,C.byref(n),pds)
PD=C.c_void_p(pds[0])
# rodziny kolejek: szukamy flagi SPARSE_BINDING (0x8)
class QF(C.Structure): _fields_=[("flags",u32),("count",u32),("ts",u32),("g",u32*3)]
qn=u32(0); vk.vkGetPhysicalDeviceQueueFamilyProperties(PD,C.byref(qn),None); qf=(QF*qn.value)(); vk.vkGetPhysicalDeviceQueueFamilyProperties(PD,C.byref(qn),qf)
fam=[i for i in range(qn.value) if qf[i].flags & 0x8 and qf[i].flags & 0x1]
print("rodziny kolejek:",[hex(qf[i].flags) for i in range(qn.value)],"z SPARSE+GRAPHICS:",fam)
if not fam: raise SystemExit("brak rodziny z SPARSE_BINDING")
F=fam[0]
prio=C.c_float(1.0)
QCI=st("QCI",[("sType",u32),("pNext",vp),("flags",u32),("fam",u32),("count",u32),("prio",C.POINTER(C.c_float))])
Feat=st("Feat",[("f",u32*55)]); feats=Feat(); feats.f[44]=1  # sparseBinding
qci=QCI(2,None,0,F,1,C.pointer(prio))
DCI=st("DCI",[("sType",u32),("pNext",vp),("flags",u32),("qc",u32),("q",C.POINTER(QCI)),("lc",u32),("l",vp),("ec",u32),("e",vp),("ef",C.POINTER(Feat))])
dev=vp(); r=vk.vkCreateDevice(PD,C.byref(DCI(3,None,0,1,C.pointer(qci),0,None,0,None,C.pointer(feats))),None,C.byref(dev)); print("urządzenie",r)
vk.vkGetDeviceProcAddr.restype=vp
def fn(name,restype=i32,*args):
    p=vk.vkGetDeviceProcAddr(dev,name.encode()); return C.CFUNCTYPE(restype,*args)(p)
q=vp(); fn("vkGetDeviceQueue",None,vp,u32,u32,C.POINTER(vp))(dev,F,0,C.byref(q))
# bufor rzadki 4 MiB
BCI=st("BCI",[("sType",u32),("pNext",vp),("flags",u32),("size",u64),("usage",u32),("sm",u32),("qc",u32),("qf",vp)])
buf=u64(); r=fn("vkCreateBuffer",i32,vp,C.POINTER(BCI),vp,C.POINTER(u64))(dev,C.byref(BCI(12,None,0x1,4<<20,0x20|0x2,0,0,None)),None,C.byref(buf)); print("bufor sparse:",r)
MR=st("MR",[("size",u64),("align",u64),("bits",u32)]); mr=MR()
fn("vkGetBufferMemoryRequirements",None,vp,u64,C.POINTER(MR))(dev,buf,C.byref(mr)); print("wymagania: rozmiar",mr.size,"wyrównanie",mr.align,"typy",hex(mr.bits))
mt=next(i for i in range(32) if mr.bits>>i&1)
MAI=st("MAI",[("sType",u32),("pNext",vp),("size",u64),("type",u32)])
mem=u64(); r=fn("vkAllocateMemory",i32,vp,C.POINTER(MAI),vp,C.POINTER(u64))(dev,C.byref(MAI(5,None,mr.size,mt)),None,C.byref(mem)); print("pamięć:",r)
SMB=st("SMB",[("resOff",u64),("size",u64),("mem",u64),("memOff",u64),("flags",u32)])
SBMBI=st("SBMBI",[("buffer",u64),("count",u32),("binds",C.POINTER(SMB))])
bind=SMB(0,mr.size,mem.value,0,0); sb=SBMBI(buf.value,1,C.pointer(bind))
BSI=st("BSI",[("sType",u32),("pNext",vp),("wc",u32),("ws",vp),("bbc",u32),("bb",C.POINTER(SBMBI)),("iobc",u32),("iob",vp),("ibc",u32),("ib",vp),("sc",u32),("ss",vp)])
bsi=BSI(1000-1000+7,None,0,None,1,C.pointer(sb),0,None,0,None,0,None)  # VK_STRUCTURE_TYPE_BIND_SPARSE_INFO = 7
r=fn("vkQueueBindSparse",i32,vp,u32,C.POINTER(BSI),u64)(q,1,C.byref(bsi),0); print("vkQueueBindSparse:",r)
r=fn("vkQueueWaitIdle",i32,vp)(q); print("vkQueueWaitIdle:",r)
# odwiązanie (memory = 0)
bind0=SMB(0,mr.size,0,0,0); sb0=SBMBI(buf.value,1,C.pointer(bind0)); bsi0=BSI(7,None,0,None,1,C.pointer(sb0),0,None,0,None,0,None)
r=fn("vkQueueBindSparse",i32,vp,u32,C.POINTER(BSI),u64)(q,1,C.byref(bsi0),0); print("odwiązanie:",r)
print("vkQueueWaitIdle:",fn("vkQueueWaitIdle",i32,vp)(q))

# ---- obraz rzadki 2D RGBA8 1024x1024 (SPARSE_BINDING | SPARSE_RESIDENCY) ----
ICI=st("ICI",[("sType",u32),("pNext",vp),("flags",u32),("type",u32),("fmt",u32),("w",u32),("h",u32),("d",u32),("mips",u32),("layers",u32),("samples",u32),("tiling",u32),("usage",u32),("sharing",u32),("qfc",u32),("qf",vp),("layout",u32)])
img=u64(); r=fn("vkCreateImage",i32,vp,C.POINTER(ICI),vp,C.POINTER(u64))(dev,C.byref(ICI(14,None,0x3,1,37,1024,1024,1,1,1,1,0,0x4|0x2,0,0,None,0)),None,C.byref(img)); print("obraz sparse:",r)
SIMR=st("SIMR",[("aspect",u32),("gx",u32),("gy",u32),("gz",u32),("fflags",u32),("firstLod",u32),("tailSize",u64),("tailOff",u64),("tailStride",u64)])
cnt=u32(0); fn("vkGetImageSparseMemoryRequirements",None,vp,u64,C.POINTER(u32),vp)(dev,img,C.byref(cnt),None); print("sparse memory requirements: liczba",cnt.value)
reqs=(SIMR*max(cnt.value,1))()
fn("vkGetImageSparseMemoryRequirements",None,vp,u64,C.POINTER(u32),C.POINTER(SIMR))(dev,img,C.byref(cnt),reqs)
for i in range(cnt.value): print("  [%d] aspekt %#x granularność %dx%dx%d flagi %#x mipTail: lod %d rozmiar %d offset %d"%(i,reqs[i].aspect,reqs[i].gx,reqs[i].gy,reqs[i].gz,reqs[i].fflags,reqs[i].firstLod,reqs[i].tailSize,reqs[i].tailOff))
gx,gy,gz=(reqs[0].gx,reqs[0].gy,reqs[0].gz) if cnt.value else (128,128,1)
mr2=MR(); fn("vkGetImageMemoryRequirements",None,vp,u64,C.POINTER(MR))(dev,img,C.byref(mr2)); print("wymagania obrazu: rozmiar",mr2.size,"wyrównanie",mr2.align)
mt2=next(i for i in range(32) if mr2.bits>>i&1)
tile=u64(); r=fn("vkAllocateMemory",i32,vp,C.POINTER(MAI),vp,C.POINTER(u64))(dev,C.byref(MAI(5,None,65536,mt2)),None,C.byref(tile)); print("pamięć kafelka:",r)
ISB=st("ISB",[("aspect",u32),("mip",u32),("layer",u32),("ox",i32),("oy",i32),("oz",i32),("ex",u32),("ey",u32),("ez",u32),("pad",u32),("mem",u64),("memOff",u64),("flags",u32),("pad2",u32)])
ib=ISB(1,0,0,0,0,0,gx,gy,gz,0,tile.value,0,0,0)
IBI=st("IBI",[("image",u64),("count",u32),("binds",C.POINTER(ISB))]); ibi=IBI(img.value,1,C.pointer(ib))
bsi2=BSI(7,None,0,None,0,None,0,None,1,C.cast(C.pointer(ibi),vp),0,None)
r=fn("vkQueueBindSparse",i32,vp,u32,C.POINTER(BSI),u64)(q,1,C.byref(bsi2),0); print("vkQueueBindSparse (kafelek obrazu):",r)
print("vkQueueWaitIdle:",fn("vkQueueWaitIdle",i32,vp)(q))

# ---- zapytanie o formaty rzadkie z tablicą (druga część dwukrokowego wywołania) ----
SFP=st("SFP",[("aspect",u32),("gx",u32),("gy",u32),("gz",u32),("flags",u32)])
for fmt,nm in ((37,"R8G8B8A8_UNORM"),(138,"format138")):
    c=u32(0); vk.vkGetPhysicalDeviceSparseImageFormatProperties(PD,fmt,1,1,0x7,0,C.byref(c),None)
    arr=(SFP*max(c.value,1))(); c2=u32(c.value)
    vk.vkGetPhysicalDeviceSparseImageFormatProperties(PD,fmt,1,1,0x7,0,C.byref(c2),arr)
    print("formaty rzadkie",nm,"liczba",c.value,"->",[(hex(arr[i].aspect),arr[i].gx,arr[i].gy,arr[i].gz,hex(arr[i].flags)) for i in range(c2.value)])
