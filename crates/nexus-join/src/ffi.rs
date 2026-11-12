//! The Lemonade Nexus SDK C API (lemonade_nexus.h), loaded at run time.
//!
//! The SDK is a C++ shared library (liblemonade_nexus_sdk.so). It is opened with dlopen so the app
//! builds and runs without it: no SDK means the Nexus section says so and nothing else changes.

use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};

pub type Client = *mut c_void;
pub type Identity = *mut c_void;

type FnFree = unsafe extern "C" fn(*mut c_char);
type FnSetCa = unsafe extern "C" fn(*const c_char) -> c_int;
type FnCreateTls = unsafe extern "C" fn(*const c_char, u16) -> Client;
type FnDestroy = unsafe extern "C" fn(Client);
type FnIdGenerate = unsafe extern "C" fn() -> Identity;
type FnIdLoad = unsafe extern "C" fn(*const c_char) -> Identity;
type FnIdSave = unsafe extern "C" fn(Identity, *const c_char) -> c_int;
type FnIdPubkey = unsafe extern "C" fn(Identity) -> *mut c_char;
type FnIdDestroy = unsafe extern "C" fn(Identity);
type FnSetIdentity = unsafe extern "C" fn(Client, Identity) -> c_int;
type FnSetStr = unsafe extern "C" fn(Client, *const c_char) -> c_int;
type FnSetGenesis = unsafe extern "C" fn(Client, *const c_char, u64) -> c_int;
type FnJoin = unsafe extern "C" fn(Client, *const c_char, *const c_char, *mut *mut c_char) -> c_int;
type FnOutJson = unsafe extern "C" fn(Client, *mut *mut c_char) -> c_int;
type FnClient = unsafe extern "C" fn(Client) -> c_int;
type FnOpenEgress = unsafe extern "C" fn(Client, *const c_char, u16, *mut u16) -> c_int;
type FnSetAddressMap = unsafe extern "C" fn(*const c_char) -> c_int;

/// Function table of one loaded SDK.
pub struct Sdk {
    _lib: Library,
    pub path: PathBuf,
    free: FnFree,
    set_ca: FnSetCa,
    create_tls: FnCreateTls,
    destroy: FnDestroy,
    id_generate: FnIdGenerate,
    id_load: FnIdLoad,
    id_save: FnIdSave,
    id_pubkey: FnIdPubkey,
    id_destroy: FnIdDestroy,
    set_identity: FnSetIdentity,
    set_genesis: FnSetGenesis,
    set_device_name: FnSetStr,
    set_link_token: FnSetStr,
    join: FnJoin,
    leave: FnOutJson,
    mesh_enable: FnClient,
    mesh_disable: FnClient,
    mesh_status: FnOutJson,
    mesh_peers: FnOutJson,
    mesh_refresh: FnClient,
    resume: FnClient,
    suspend: FnClient,
    open_egress: FnOpenEgress,
    /// Optional: only SDK builds with the client address map (see README) have it.
    set_address_map: Option<FnSetAddressMap>,
}

// The SDK's C API is documented as callable from several threads (the Dart client uses it from
// worker isolates); the table itself is plain function pointers.
unsafe impl Send for Sdk {}
unsafe impl Sync for Sdk {}

/// Error code names from ln_error_t.
pub fn code_name(c: c_int) -> &'static str {
    match c {
        0 => "ok",
        -1 => "null argument",
        -2 => "server not reached",
        -3 => "refused (auth)",
        -4 => "not found",
        -5 => "rejected",
        -6 => "no identity",
        -7 => "malformed reply",
        -8 => "unsupported",
        -9 => "port in use",
        _ => "internal error",
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl Sdk {
    /// Open the first SDK found in `candidates` (file paths or bare library names for the loader).
    pub fn open(candidates: &[PathBuf]) -> Result<Sdk, String> {
        let mut errors = Vec::new();
        for c in candidates {
            match unsafe { Library::new(c) } {
                Ok(lib) => return Self::bind(lib, c.clone()),
                Err(e) => errors.push(format!("{}: {e}", c.display())),
            }
        }
        Err(if errors.is_empty() { "no SDK path configured".into() } else { errors.join("; ") })
    }

    fn bind(lib: Library, path: PathBuf) -> Result<Sdk, String> {
        unsafe {
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes()).map_err(|e| format!("{}: {e}", $name))?
                };
            }
            let set_address_map = lib.get::<FnSetAddressMap>(b"ln_set_address_map\0").ok().map(|s| *s);
            Ok(Sdk {
                free: sym!("ln_free"),
                set_ca: sym!("ln_set_ca_bundle_path"),
                create_tls: sym!("ln_create_tls"),
                destroy: sym!("ln_destroy"),
                id_generate: sym!("ln_identity_generate"),
                id_load: sym!("ln_identity_load"),
                id_save: sym!("ln_identity_save"),
                id_pubkey: sym!("ln_identity_pubkey"),
                id_destroy: sym!("ln_identity_destroy"),
                set_identity: sym!("ln_set_identity"),
                set_genesis: sym!("ln_set_genesis_key"),
                set_device_name: sym!("ln_set_device_name"),
                set_link_token: sym!("ln_set_link_token"),
                join: sym!("ln_join_network"),
                leave: sym!("ln_leave_network"),
                mesh_enable: sym!("ln_mesh_enable"),
                mesh_disable: sym!("ln_mesh_disable"),
                mesh_status: sym!("ln_mesh_status"),
                mesh_peers: sym!("ln_mesh_peers"),
                mesh_refresh: sym!("ln_mesh_refresh"),
                resume: sym!("ln_resume"),
                suspend: sym!("ln_suspend"),
                open_egress: sym!("ln_mesh_open_egress"),
                set_address_map,
                path,
                _lib: lib,
            })
        }
    }

    fn take(&self, p: *mut c_char) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { (self.free)(p) };
        s
    }

    pub fn has_address_map(&self) -> bool {
        self.set_address_map.is_some()
    }

    pub fn set_address_map(&self, json: &str) -> Result<(), String> {
        let f = self.set_address_map.ok_or("this SDK build has no address map (ln_set_address_map)")?;
        let c = unsafe { f(cstr(json).as_ptr()) };
        if c == 0 { Ok(()) } else { Err(format!("address map refused: {}", code_name(c))) }
    }

    pub fn set_ca_bundle(&self, path: &Path) -> Result<(), String> {
        let c = unsafe { (self.set_ca)(cstr(&path.to_string_lossy()).as_ptr()) };
        if c == 0 { Ok(()) } else { Err(format!("CA bundle: {}", code_name(c))) }
    }

    pub fn create_tls(&self, host: &str, port: u16) -> Result<Client, String> {
        let p = unsafe { (self.create_tls)(cstr(host).as_ptr(), port) };
        if p.is_null() { Err(format!("could not create a client for {host}:{port}")) } else { Ok(p) }
    }

    pub fn destroy(&self, c: Client) {
        if !c.is_null() {
            unsafe { (self.destroy)(c) }
        }
    }

    /// Load the identity at `path`, or make one and save it there (mode 0600 is the caller's job).
    pub fn identity_load_or_create(&self, path: &Path) -> Result<(Identity, bool), String> {
        let p = cstr(&path.to_string_lossy());
        if path.exists() {
            let id = unsafe { (self.id_load)(p.as_ptr()) };
            if !id.is_null() {
                return Ok((id, false));
            }
            return Err(format!("identity file {} could not be read", path.display()));
        }
        let id = unsafe { (self.id_generate)() };
        if id.is_null() {
            return Err("could not generate an identity".into());
        }
        let c = unsafe { (self.id_save)(id, p.as_ptr()) };
        if c != 0 {
            unsafe { (self.id_destroy)(id) };
            return Err(format!("could not save the identity: {}", code_name(c)));
        }
        Ok((id, true))
    }

    pub fn identity_pubkey(&self, id: Identity) -> String {
        self.take(unsafe { (self.id_pubkey)(id) })
    }

    pub fn identity_destroy(&self, id: Identity) {
        if !id.is_null() {
            unsafe { (self.id_destroy)(id) }
        }
    }

    pub fn set_identity(&self, c: Client, id: Identity) -> Result<(), String> {
        let r = unsafe { (self.set_identity)(c, id) };
        if r == 0 { Ok(()) } else { Err(format!("identity: {}", code_name(r))) }
    }

    pub fn set_genesis(&self, c: Client, key_b64: &str) -> Result<(), String> {
        let r = unsafe { (self.set_genesis)(c, cstr(key_b64).as_ptr(), 0) };
        if r == 0 { Ok(()) } else { Err(format!("genesis key: {}", code_name(r))) }
    }

    pub fn set_device_name(&self, c: Client, name: &str) -> Result<(), String> {
        let r = unsafe { (self.set_device_name)(c, cstr(name).as_ptr()) };
        if r == 0 { Ok(()) } else { Err(format!("device name refused: {}", code_name(r))) }
    }

    pub fn set_link_token(&self, c: Client, token: &str) -> Result<(), String> {
        let r = unsafe { (self.set_link_token)(c, cstr(token).as_ptr()) };
        if r == 0 { Ok(()) } else { Err(format!("invite refused by the SDK: {}", code_name(r))) }
    }

    /// ln_join_network with the identity (empty username and password). Returns (code, json).
    pub fn join(&self, c: Client) -> (c_int, String) {
        let mut out: *mut c_char = std::ptr::null_mut();
        let empty = cstr("");
        let r = unsafe { (self.join)(c, empty.as_ptr(), empty.as_ptr(), &mut out) };
        (r, self.take(out))
    }

    pub fn leave(&self, c: Client) -> (c_int, String) {
        self.out_json(self.leave, c)
    }

    fn out_json(&self, f: FnOutJson, c: Client) -> (c_int, String) {
        let mut out: *mut c_char = std::ptr::null_mut();
        let r = unsafe { f(c, &mut out) };
        (r, self.take(out))
    }

    pub fn mesh_enable(&self, c: Client) -> c_int {
        unsafe { (self.mesh_enable)(c) }
    }
    pub fn mesh_disable(&self, c: Client) -> c_int {
        unsafe { (self.mesh_disable)(c) }
    }
    pub fn mesh_status(&self, c: Client) -> (c_int, String) {
        self.out_json(self.mesh_status, c)
    }
    pub fn mesh_peers(&self, c: Client) -> (c_int, String) {
        self.out_json(self.mesh_peers, c)
    }
    pub fn mesh_refresh(&self, c: Client) -> c_int {
        unsafe { (self.mesh_refresh)(c) }
    }
    pub fn resume(&self, c: Client) -> c_int {
        unsafe { (self.resume)(c) }
    }
    pub fn suspend(&self, c: Client) -> c_int {
        unsafe { (self.suspend)(c) }
    }
    pub fn open_egress(&self, c: Client, ip: &str, port: u16) -> Result<u16, String> {
        let mut lp: u16 = 0;
        let r = unsafe { (self.open_egress)(c, cstr(ip).as_ptr(), port, &mut lp) };
        if r == 0 && lp != 0 { Ok(lp) } else { Err(format!("mesh route to {ip}:{port}: {}", code_name(r))) }
    }
}
