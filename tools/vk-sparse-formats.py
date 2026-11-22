#!/usr/bin/env python3
# Test zapytań o pamięć rzadką przez nasz ICD (libvulkan + VK_DRIVER_FILES). Wypisuje liczbę właściwości formatów rzadkich i cechy urządzenia.
import ctypes as C, os, sys, json
vk = C.CDLL("libvulkan.so.1")
u32, u64, vp = C.c_uint32, C.c_uint64, C.c_void_p
class AppInfo(C.Structure):
    _fields_ = [("sType",u32),("pNext",vp),("pApplicationName",C.c_char_p),("applicationVersion",u32),("pEngineName",C.c_char_p),("engineVersion",u32),("apiVersion",u32)]
class InstInfo(C.Structure):
    _fields_ = [("sType",u32),("pNext",vp),("flags",u32),("pApplicationInfo",C.POINTER(AppInfo)),("enabledLayerCount",u32),("ppEnabledLayerNames",vp),("enabledExtensionCount",u32),("ppEnabledExtensionNames",vp)]
app = AppInfo(0, None, b"sparse-test", 1, None, 0, (1<<22)|(3<<12))
ci = InstInfo(1, None, 0, C.pointer(app), 0, None, 0, None)
inst = vp()
r = vk.vkCreateInstance(C.byref(ci), None, C.byref(inst)); print("vkCreateInstance:", r)
n = u32(0); vk.vkEnumeratePhysicalDevices(inst, C.byref(n), None)
pds = (vp*n.value)(); vk.vkEnumeratePhysicalDevices(inst, C.byref(n), pds)
for i in range(n.value):
    PD = C.c_void_p(pds[i])
    class Props(C.Structure): _fields_=[("apiVersion",u32),("driverVersion",u32),("vendorID",u32),("deviceID",u32),("deviceType",u32),("deviceName",C.c_char*256),("rest",C.c_ubyte*600)]
    p = Props(); vk.vkGetPhysicalDeviceProperties(PD, C.byref(p)); print("urządzenie", i, p.deviceName.decode(), "typ", p.deviceType)
    class Feat(C.Structure): _fields_=[("f",u32*55)]
    f = Feat(); vk.vkGetPhysicalDeviceFeatures(PD, C.byref(f))
    names = ["robustBufferAccess","fullDrawIndexUint32","imageCubeArray","independentBlend","geometryShader","tessellationShader","sampleRateShading","dualSrcBlend","logicOp","multiDrawIndirect","drawIndirectFirstInstance","depthClamp","depthBiasClamp","fillModeNonSolid","depthBounds","wideLines","largePoints","alphaToOne","multiViewport","samplerAnisotropy","textureCompressionETC2","textureCompressionASTC_LDR","textureCompressionBC","occlusionQueryPrecise","pipelineStatisticsQuery","vertexPipelineStoresAndAtomics","fragmentStoresAndAtomics","shaderTessellationAndGeometryPointSize","shaderImageGatherExtended","shaderStorageImageExtendedFormats","shaderStorageImageMultisample","shaderStorageImageReadWithoutFormat","shaderStorageImageWriteWithoutFormat","shaderUniformBufferArrayDynamicIndexing","shaderSampledImageArrayDynamicIndexing","shaderStorageBufferArrayDynamicIndexing","shaderStorageImageArrayDynamicIndexing","shaderClipDistance","shaderCullDistance","shaderFloat64","shaderInt64","shaderInt16","shaderResourceResidency","shaderResourceMinLod","sparseBinding","sparseResidencyBuffer","sparseResidencyImage2D","sparseResidencyImage3D","sparseResidency2Samples","sparseResidency4Samples","sparseResidency8Samples","sparseResidency16Samples","sparseResidencyAliased","variableMultisampleRate","inheritedQueries"]
    print("  funkcje rzadkie:", {nm: f.f[k] for k,nm in enumerate(names) if nm.startswith("sparse") or nm=="shaderResourceResidency"})
    for fmt,fname in ((37,"R8G8B8A8_UNORM"),(138,"format138(BC7?)"),(131,"BC1_RGB"),(43,"R8G8B8A8_SRGB")):
        c = u32(0); vk.vkGetPhysicalDeviceSparseImageFormatProperties(PD, fmt, 1, 1, 0x7, 0, C.byref(c), None)
        print("  sparse format props", fname, "liczba:", c.value)
