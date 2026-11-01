//! Logika sesji: uwierzytelnianie, własność uchwytów i mapowanie żądań na `GpuBackend`.
//! Nie zależy od Vulkana (backend jest cechą), więc da się ją w pełni testować atrapą.

use gpu_proto::msg::{PdFunc, Request, Response, MAX_NAME_LEN, PROTOCOL_VERSION};
use gpu_wire::Encoder;

/// `VK_ERROR_UNKNOWN`: kod dla odrzuconych żądań (nieznany uchwyt, limit).
pub const ERR_UNKNOWN: i32 = -13;
/// `VK_ERROR_INITIALIZATION_FAILED`.
pub const ERR_INIT_FAILED: i32 = -3;
/// Kod 0 w `Response::Error` oznacza błąd protokołu (nie `VkResult`).
pub const ERR_PROTOCOL: i32 = 0;
/// Maksymalna liczba jednocześnie żywych instancji w jednej sesji.
pub const MAX_INSTANCES: usize = 16;

/// Backend karty graficznej. Błąd to ujemny `VkResult`.
pub trait GpuBackend {
    fn instance_version(&mut self) -> Result<u32, i32>;
    fn instance_extensions(&mut self) -> Result<Vec<(String, u32)>, i32>;
    fn create_instance(&mut self, api_version: u32, extensions: &[String]) -> Result<u64, i32>;
    fn destroy_instance(&mut self, instance: u64) -> Result<(), i32>;
    fn enum_physical_devices(&mut self, instance: u64) -> Result<Vec<u64>, i32>;
    fn get_physical_device(&mut self, pdev: u64, func: PdFunc, arg: u32, count: u32, chain: &[u32]) -> Result<Vec<u8>, i32>;
    fn device_extensions(&mut self, pdev: u64) -> Result<Vec<(String, u32)>, i32>;
}

/// Kodowanie listy rozszerzeń w `Response::Data`: varint liczby, potem (napis, u32 specVersion).
pub fn encode_extensions(list: &[(String, u32)]) -> Vec<u8> {
    todo!()
}

/// Odwrotność `encode_extensions`; `None` przy błędnych danych (ucięte, za długie nazwy, nadmiar).
pub fn decode_extensions(bytes: &[u8]) -> Option<Vec<(String, u32)>> {
    todo!()
}

/// Jedna sesja klienta. Dopóki `Hello` nie przejdzie, wszystkie inne żądania są odrzucane.
pub struct Session<B: GpuBackend> {
    backend: B,
    token: Vec<u8>,
    authed: bool,
    /// instancja -> fizyczne karty zwrócone przez `EnumPhysicalDevices` (tylko takie wolno odpytywać).
    instances: Vec<(u64, Vec<u64>)>,
}

impl<B: GpuBackend> Session<B> {
    pub fn new(backend: B, token: Vec<u8>) -> Self {
        Session { backend, token, authed: false, instances: Vec::new() }
    }

    pub fn is_authenticated(&self) -> bool {
        self.authed
    }

    pub fn handle(&mut self, req: Request) -> Response {
        todo!()
    }

    /// Wywoływane po rozłączeniu: niszczy wszystkie instancje, które sesja jeszcze trzyma.
    pub fn cleanup(&mut self) {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Calls {
        log: Vec<String>,
        next_handle: u64,
    }

    #[derive(Clone, Default)]
    struct Mock {
        calls: Rc<RefCell<Calls>>,
        fail_create: bool,
    }

    impl GpuBackend for Mock {
        fn instance_version(&mut self) -> Result<u32, i32> {
            self.calls.borrow_mut().log.push("version".into());
            Ok(0x0040_3000)
        }
        fn instance_extensions(&mut self) -> Result<Vec<(String, u32)>, i32> {
            Ok(vec![("VK_KHR_surface".into(), 25), ("VK_EXT_debug_utils".into(), 2)])
        }
        fn create_instance(&mut self, api: u32, ext: &[String]) -> Result<u64, i32> {
            if self.fail_create {
                return Err(-9);
            }
            let mut c = self.calls.borrow_mut();
            c.next_handle += 100;
            let h = c.next_handle;
            c.log.push(format!("create {api:#x} {} -> {h}", ext.join(",")));
            Ok(h)
        }
        fn destroy_instance(&mut self, i: u64) -> Result<(), i32> {
            self.calls.borrow_mut().log.push(format!("destroy {i}"));
            Ok(())
        }
        fn enum_physical_devices(&mut self, i: u64) -> Result<Vec<u64>, i32> {
            Ok(vec![i + 1, i + 2])
        }
        fn get_physical_device(&mut self, p: u64, f: PdFunc, arg: u32, count: u32, chain: &[u32]) -> Result<Vec<u8>, i32> {
            self.calls.borrow_mut().log.push(format!("get {p} {f:?} {arg} {count} {chain:?}"));
            Ok(vec![0xAB; 4])
        }
        fn device_extensions(&mut self, _p: u64) -> Result<Vec<(String, u32)>, i32> {
            Ok(vec![("VK_KHR_swapchain".into(), 70)])
        }
    }

    fn session() -> (Session<Mock>, Rc<RefCell<Calls>>) {
        let m = Mock::default();
        let calls = m.calls.clone();
        (Session::new(m, b"sekret".to_vec()), calls)
    }

    fn hello(token: &[u8]) -> Request {
        Request::Hello { version: PROTOCOL_VERSION, token: token.to_vec() }
    }

    fn authed() -> (Session<Mock>, Rc<RefCell<Calls>>) {
        let (mut s, c) = session();
        assert_eq!(s.handle(hello(b"sekret")), Response::HelloOk { version: PROTOCOL_VERSION });
        (s, c)
    }

    fn is_err(r: &Response) -> bool {
        matches!(r, Response::Error { .. })
    }

    #[test]
    fn extensions_encoding_roundtrips_and_rejects_garbage() {
        let l = vec![("VK_KHR_surface".to_string(), 25), ("x".to_string(), 0)];
        assert_eq!(decode_extensions(&encode_extensions(&l)), Some(l));
        assert_eq!(decode_extensions(&encode_extensions(&[])), Some(vec![]));
        let b = encode_extensions(&[("VK_KHR_surface".into(), 25)]);
        for cut in 0..b.len() {
            assert_eq!(decode_extensions(&b[..cut]), None, "ucięte do {cut}");
        }
        let mut extra = b.clone();
        extra.push(0);
        assert_eq!(decode_extensions(&extra), None, "nadmiarowy bajt");
        let long = encode_extensions(&[("a".repeat(MAX_NAME_LEN + 1), 1)]);
        assert_eq!(decode_extensions(&long), None, "za długa nazwa");
        let mut e = Encoder::new();
        e.put_varint(u64::MAX);
        assert_eq!(decode_extensions(&e.finish()), None, "wrogi licznik");
    }

    #[test]
    fn nothing_works_before_hello_and_backend_is_untouched() {
        let (mut s, calls) = session();
        for r in [
            Request::EnumInstanceVersion,
            Request::EnumInstanceExtensions,
            Request::CreateInstance { api_version: 1, extensions: vec![] },
            Request::EnumPhysicalDevices { instance: 1 },
            Request::DestroyInstance { instance: 1 },
            Request::EnumDeviceExtensions { pdev: 1 },
        ] {
            assert!(is_err(&s.handle(r.clone())), "{r:?}");
        }
        assert!(calls.borrow().log.is_empty(), "{:?}", calls.borrow().log);
        assert!(!s.is_authenticated());
    }

    #[test]
    fn wrong_token_or_version_fails_and_stays_unauthenticated() {
        let (mut s, _) = session();
        assert!(is_err(&s.handle(hello(b"zly"))));
        assert!(is_err(&s.handle(hello(b""))));
        assert!(is_err(&s.handle(hello(b"sekret1"))), "token dłuższy o znak");
        assert!(is_err(&s.handle(hello(b"sekre"))), "token krótszy o znak");
        assert!(is_err(&s.handle(Request::Hello { version: PROTOCOL_VERSION + 1, token: b"sekret".to_vec() })));
        assert!(!s.is_authenticated());
        assert!(is_err(&s.handle(Request::EnumInstanceVersion)));
        assert_eq!(s.handle(hello(b"sekret")), Response::HelloOk { version: PROTOCOL_VERSION }, "poprawny Hello po błędnych");
        assert!(s.is_authenticated());
    }

    #[test]
    fn empty_configured_token_never_authenticates() {
        let mut s = Session::new(Mock::default(), Vec::new());
        assert!(is_err(&s.handle(hello(b""))), "pusty token w konfiguracji nie może wpuszczać");
    }

    #[test]
    fn second_hello_after_success_is_an_error() {
        let (mut s, _) = authed();
        assert!(is_err(&s.handle(hello(b"sekret"))));
    }

    #[test]
    fn version_and_extension_queries_after_hello() {
        let (mut s, _) = authed();
        assert_eq!(s.handle(Request::EnumInstanceVersion), Response::U32(0x0040_3000));
        let Response::Data(d) = s.handle(Request::EnumInstanceExtensions) else { panic!() };
        assert_eq!(decode_extensions(&d).unwrap()[0], ("VK_KHR_surface".to_string(), 25));
    }

    #[test]
    fn instance_lifecycle_and_device_queries() {
        let (mut s, calls) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance { api_version: 0x0040_3000, extensions: vec!["VK_KHR_surface".into()] }) else { panic!() };
        let Response::Handles(pds) = s.handle(Request::EnumPhysicalDevices { instance: inst }) else { panic!() };
        assert_eq!(pds, vec![inst + 1, inst + 2]);
        let r = s.handle(Request::GetPhysicalDevice { pdev: pds[0], func: PdFunc::Properties2, arg: 0, count: 0, chain: vec![1000059001] });
        assert_eq!(r, Response::Data(vec![0xAB; 4]));
        assert!(calls.borrow().log.iter().any(|l| l.contains("get") && l.contains("Properties2") && l.contains("1000059001")));
        let Response::Data(d) = s.handle(Request::EnumDeviceExtensions { pdev: pds[1] }) else { panic!() };
        assert_eq!(decode_extensions(&d).unwrap()[0].0, "VK_KHR_swapchain");
        assert_eq!(s.handle(Request::DestroyInstance { instance: inst }), Response::U32(0));
        assert!(calls.borrow().log.contains(&format!("destroy {inst}")));
    }

    #[test]
    fn unknown_handles_are_rejected_before_reaching_the_backend() {
        let (mut s, calls) = authed();
        assert!(is_err(&s.handle(Request::EnumPhysicalDevices { instance: 12345 })));
        assert!(is_err(&s.handle(Request::DestroyInstance { instance: 12345 })));
        assert!(is_err(&s.handle(Request::GetPhysicalDevice { pdev: 777, func: PdFunc::Properties, arg: 0, count: 0, chain: vec![] })));
        assert!(is_err(&s.handle(Request::EnumDeviceExtensions { pdev: 777 })));
        assert!(!calls.borrow().log.iter().any(|l| l.starts_with("get") || l.starts_with("destroy")), "{:?}", calls.borrow().log);
    }

    #[test]
    fn physical_device_not_yet_enumerated_is_rejected() {
        let (mut s, _) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] }) else { panic!() };
        // Karta inst+1 zostałaby zwrócona, ale jeszcze nie wyliczono.
        assert!(is_err(&s.handle(Request::GetPhysicalDevice { pdev: inst + 1, func: PdFunc::Properties, arg: 0, count: 0, chain: vec![] })));
    }

    #[test]
    fn destroying_an_instance_revokes_its_physical_devices_and_double_destroy_fails() {
        let (mut s, _) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] }) else { panic!() };
        let Response::Handles(pds) = s.handle(Request::EnumPhysicalDevices { instance: inst }) else { panic!() };
        assert!(!is_err(&s.handle(Request::DestroyInstance { instance: inst })));
        assert!(is_err(&s.handle(Request::GetPhysicalDevice { pdev: pds[0], func: PdFunc::Properties, arg: 0, count: 0, chain: vec![] })));
        assert!(is_err(&s.handle(Request::DestroyInstance { instance: inst })));
    }

    #[test]
    fn repeated_enumeration_does_not_duplicate_or_break_access() {
        let (mut s, _) = authed();
        let Response::Handle(inst) = s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] }) else { panic!() };
        let a = s.handle(Request::EnumPhysicalDevices { instance: inst });
        let b = s.handle(Request::EnumPhysicalDevices { instance: inst });
        assert_eq!(a, b);
        assert!(!is_err(&s.handle(Request::EnumDeviceExtensions { pdev: inst + 1 })));
    }

    #[test]
    fn instance_limit_is_enforced_and_freed_by_destroy() {
        let (mut s, _) = authed();
        let mut handles = Vec::new();
        for _ in 0..MAX_INSTANCES {
            let Response::Handle(h) = s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] }) else { panic!() };
            handles.push(h);
        }
        assert!(is_err(&s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] })));
        s.handle(Request::DestroyInstance { instance: handles[0] });
        assert!(!is_err(&s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] })));
    }

    #[test]
    fn backend_error_is_passed_through_with_its_code() {
        let m = Mock { fail_create: true, ..Default::default() };
        let mut s = Session::new(m, b"t".to_vec());
        s.handle(Request::Hello { version: PROTOCOL_VERSION, token: b"t".to_vec() });
        match s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] }) {
            Response::Error { code, .. } => assert_eq!(code, -9),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cleanup_destroys_all_remaining_instances_once() {
        let (mut s, calls) = authed();
        let Response::Handle(a) = s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] }) else { panic!() };
        let Response::Handle(b) = s.handle(Request::CreateInstance { api_version: 1, extensions: vec![] }) else { panic!() };
        s.handle(Request::DestroyInstance { instance: a });
        s.cleanup();
        s.cleanup();
        let destroys: Vec<_> = calls.borrow().log.iter().filter(|l| l.starts_with("destroy")).cloned().collect();
        assert_eq!(destroys, vec![format!("destroy {a}"), format!("destroy {b}")]);
    }
}
