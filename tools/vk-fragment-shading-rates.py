import ctypes as C
vk=C.CDLL("libvulkan.so.1"); u32,vp=C.c_uint32,C.c_void_p
App=type("App",(C.Structure,),{"_fields_":[("s",u32),("p",vp),("n",C.c_char_p),("av",u32),("e",C.c_char_p),("ev",u32),("api",u32)]})
Inst=type("Inst",(C.Structure,),{"_fields_":[("s",u32),("p",vp),("f",u32),("app",C.POINTER(App)),("lc",u32),("l",vp),("ec",u32),("e",vp)]})
app=App(0,None,b"t",1,None,0,(1<<22)|(3<<12)); inst=vp()
print("inst",vk.vkCreateInstance(C.byref(Inst(1,None,0,C.pointer(app),0,None,0,None)),None,C.byref(inst)))
n=u32(0); vk.vkEnumeratePhysicalDevices(inst,C.byref(n),None); pds=(vp*n.value)(); vk.vkEnumeratePhysicalDevices(inst,C.byref(n),pds)
vk.vkGetInstanceProcAddr.restype=vp
f=C.CFUNCTYPE(C.c_int32,vp,C.POINTER(u32),vp)(vk.vkGetInstanceProcAddr(inst,b"vkGetPhysicalDeviceFragmentShadingRatesKHR"))
c=u32(0); print("count call:",f(C.c_void_p(pds[0]),C.byref(c),None),c.value)
Rate=type("R",(C.Structure,),{"_fields_":[("s",u32),("p",vp),("sc",u32),("w",u32),("h",u32)]})
arr=(Rate*max(c.value,1))()
for r in arr: r.s=1000226003
c2=u32(c.value); print("array call:",f(C.c_void_p(pds[0]),C.byref(c2),C.cast(arr,vp)),c2.value,[(r.sc,r.w,r.h) for r in arr[:c2.value]])
