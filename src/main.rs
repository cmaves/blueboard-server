use clap::{App, Arg};
use rustable::gatt::{
    CharFlags, DescFlags, HasChildren, LocalCharBase, LocalDescBase, LocalServiceBase, ValOrFn,
};
use rustable::{AdType, Advertisement, Bluetooth, Error as BLEError, ToUUID, MAX_APP_MTU, UUID};

use serde::{Deserialize, Serialize};
use serde_yaml;

use airboard_server::{Clip, InSyncer, OutSyncer};
use sha2::{Digest, Sha256};
use std::borrow::Borrow;
use std::cell::RefCell;
use std::collections::HashSet;
use std::env::var_os;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::thread::sleep;
use std::time::{Duration, Instant};
// use wl_clipboard_rs::paste::Error as PasteError;

const COPY_UUID: &'static str = "4981333e-2d59-43b2-8dc3-8fedee1472c5";
const READ_UUID: &'static str = "07178017-1879-451b-9bb5-3ff13bb85b70";
const WRITE_UUID: &'static str = "07178017-1879-451b-9bb5-3ff13bb85b71";

const VER_UUID: &'static str = "b05778f1-5a88-46a3-b6c8-2d154d629910";
const LEN_UUID: &'static str = "b05778f1-5a88-46a3-b6c8-2d154d629911";
const MIME_UUID: &'static str = "b05778f1-5a88-46a3-b6c8-2d154d629912";
const HASH_UUID: &'static str = "b05778f1-5a88-46a3-b6c8-2d154d629913";
//const LOC_UUID: &'static str = "b05778f1-5a88-46a3-b6c8-2d154d629912";

const VERSION: &'static str = env!("CARGO_PKG_VERSION");

fn update_clipboard(clip: &Clip) -> Result<(), std::io::Error> {
    let proc = Command::new("wl-copy")
        .arg("-t")
        .arg(clip.mime())
        .stdin(Stdio::piped())
        .spawn()?;
    proc.stdin.unwrap().write(clip.data()).map(|_| ())
}
fn resolve_mime_type(mut mimes: HashSet<String>) -> Option<String> {
    if let Some(s) = mimes.take("text/plain;charset=utf-8") {
        return Some(s);
    }
    let mut mimes: Vec<String> = mimes.into_iter().collect();
    mimes.sort_unstable();

    let mut start = binary_search(&mimes[..], "image/").unwrap_err();
    while start < mimes.len() && mimes[start].starts_with("image/") {
        let second = &mimes[start][6..];
        match second {
            "png" | "jpeg" => return Some(mimes.remove(start)),
            _ => (),
        }
        start += 1;
    }
    let mut start = binary_search(&mimes[..], "text/").unwrap_err();
    while start < mimes.len() && mimes[start].starts_with("text/") {
        let second = &mimes[start][5..];
        match second {
            "html" => return Some(mimes.remove(start)),
            _ => (),
        }
        start += 1;
    }
    return None;
}
fn binary_search<T, K>(list: &[T], k: &K) -> Result<usize, usize>
where
    T: Borrow<K>,
    K: Ord + ?Sized,
{
    list.binary_search_by(|p| p.borrow().cmp(k))
}
fn get_clipboard() -> std::io::Result<Rc<Clip>> {
    loop {
        let mime_bytes = Command::new("wl-paste").arg("-l").output()?;
        // let mimes = get_mime_types(ClipboardType::Regular, Seat::Unspecified)?;
        let mimes: HashSet<String> = std::str::from_utf8(&mime_bytes.stdout)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?
            .lines()
            .map(|s| s.to_owned())
            .collect();

        let mime = resolve_mime_type(mimes)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let out = match Command::new("wl-paste")
            .arg("-n")
            .arg("-t")
            .arg(&mime)
            .output()
        {
            Ok(out) => out.stdout,
            Err(_) => continue,
        };
        return Ok(Rc::new(Clip::new(out, mime)));

        /*
        let (mut out, _) = match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Specific(&mime))        {
            Ok(o) => o,
            Err(PasteError::NoMimeType) => continue,
            Err(e) => return Err(e)
        };
        let mut ret = vec![];
        out.read_to_end(&mut ret).map_err(|e| PasteError::WaylandCommunication(e))?;
        */
    }
}
static mut VERBOSE: u8 = 0;
fn set_verbose(level: u8) {
    unsafe {
        VERBOSE = level;
    }
}
fn verbose() -> u8 {
    unsafe { VERBOSE }
}

#[derive(Serialize, Deserialize, Default, PartialEq, Debug)]
#[serde(default)]
struct Handles {
    service: u16,
    read: u16,
    read_ver: u16,
    write: u16,
    write_ver: u16,
}
fn get_env_config_path() -> PathBuf {
    let mut path: PathBuf = var_os("HOME").unwrap().into();
    path.push(".config/airboard/handles");
    path
}
fn get_config_file<T: AsRef<Path>>(path: Option<T>) -> std::io::Result<File> {
    match path {
        Some(p) => File::open(p),
        None => {
            let path = get_env_config_path();
            File::open(path)
        }
    }
}
fn get_write_config_file<T: AsRef<Path>>(path: Option<T>) -> std::io::Result<File> {
    let mut oo = OpenOptions::new();
    oo.write(true).create(true);
    match path {
        Some(p) => oo.open(p),
        None => {
            let path = get_env_config_path();
            std::fs::create_dir_all(path.parent().unwrap())?;
            oo.open(path)
        }
    }
}
fn get_handles<T: AsRef<Path>>(path: Option<T>) -> Handles {
    let file = match get_config_file(path) {
        Ok(f) => f,
        Err(_) => return Handles::default(),
    };
    serde_yaml::from_reader(file).unwrap_or(Handles::default())
}
fn set_handles<T: AsRef<Path>>(path: Option<T>, handles: &Handles) -> std::io::Result<()> {
    let file = get_write_config_file(path)?;
    serde_yaml::to_writer(file, handles).map_err(|_| std::io::ErrorKind::Other.into())
}

fn main() {
    let parser = parser();
    let args = parser.get_matches();
    let name = match args.value_of("hostname") {
        Some(n) => n.to_string(),
        None => {
            let res = Command::new("hostname")
                .output()
                .expect("Failed to get device hostname!");
            let mut n = String::from_utf8(res.stdout).expect("Invalid hostname received!");
            n.pop();
            n
        }
    };
    let verbose = args.occurrences_of("verbose") as u8;
    set_verbose(verbose);
    let mut handles = get_handles::<&Path>(None);
    println!("Starting service with handles: {:?}", handles);

    let mut blue;
    let serv_uuid = COPY_UUID.to_uuid();
    let read_uuid = READ_UUID.to_uuid();
    let write_uuid = WRITE_UUID.to_uuid();
    let ver_uuid = VER_UUID.to_uuid();
    let (in_syncer, out_syncer) =  loop {
        blue = Bluetooth::new(
            "io.maves.airboard".to_string(),
            "/org/bluez/hci0".to_string(),
        )
        .unwrap();
        blue.verbose = verbose;
        if args.is_present("no-filter") {
            blue.set_filter(None).unwrap();
        }
        let mut copy_service = LocalServiceBase::new(&serv_uuid, true);
        copy_service.set_handle(handles.service);

        let cur_clip = match get_clipboard() {
            Ok(o) => o,
            Err(e) => {
                eprintln!("Failed to read clipboard: {:?}", e);
                Rc::new(Clip::default())
            }
        };

        let in_syncer = Rc::new(RefCell::new(InSyncer::new(cur_clip.clone())));
        let out_syncer = Rc::new(RefCell::new(OutSyncer::new(cur_clip, verbose)));

        /*
           The read and write services are from the prespective of the client. So
           for this program we read the write_char for updates from the client (typically a phone)
           and write to the read_char for updates to the client from this device.
        */
        let mut read_flags = CharFlags::default();
        // perimissions
        read_flags.secure_read = true;
        read_flags.encrypt_read = true;
        read_flags.notify = true;
        read_flags.indicate = true;
        read_flags.encrypt_write = true;
        read_flags.write_wo_response = true;
        // create read characteristic
        let mut read_char = LocalCharBase::new(&read_uuid, read_flags);
        // neable the write fd and setup the write callback
        read_char.enable_write_fd(true);
        read_char.set_handle(handles.read);

        let os_clone = out_syncer.clone();
        read_char.write_callback = Some(Box::new(move |data| {
            if verbose >= 2 {
                eprintln!(
                    "read_char.write_callback(): Read characteristic written to with: {:?}",
                    data
                );
            }
            if data.len() != 4 && data.len() != 36 {
                return Err((
                    "org.bluez.DBus.Failed".to_string(),
                    Some("Data was not 4 or 36 bytes long".to_string()),
                ));
            }
            os_clone.borrow_mut().update_pos(data);
            Ok((None, false))
        }));

        let os_clone = out_syncer.clone();
        read_char.write_val_or_fn(&mut ValOrFn::Function(Box::new(move || {
            if verbose > 2 {
                eprintln!("Read characteristic read.");
            }
            os_clone.borrow_mut().read_fn()
        })));

        // create protocol version descriptor
        let mut ver_flags = DescFlags::default();
        ver_flags.read = true;
        ver_flags.encrypt_read = true;
        ver_flags.secure_read = true;
        let mut ver_desc = LocalDescBase::new(&ver_uuid, ver_flags);
        ver_desc.vf = ValOrFn::Value([1_u8, 0][..].into());
        ver_desc.set_handle(handles.read_ver);

        /*
        let mut loc_desc = LocalDescBase::new(LOC_UUID, ver_flags);
        let os_clone = out_syncer.clone();
        loc_desc.vf = ValOrFn::Function(Box::new(move || {
            os_clone.borrow().read_loc()
        }));
        */

        let mut len_desc = LocalDescBase::new(LEN_UUID, ver_flags);
        let os_clone = out_syncer.clone();
        len_desc.vf = ValOrFn::Function(Box::new(move || RefCell::borrow(&os_clone).read_len()));

        let mut mime_desc = LocalDescBase::new(MIME_UUID, ver_flags);
        let os_clone = out_syncer.clone();
        mime_desc.vf = ValOrFn::Function(Box::new(move || RefCell::borrow(&os_clone).read_mime()));

        let mut mime_desc = LocalDescBase::new(MIME_UUID, ver_flags);
        let os_clone = out_syncer.clone();
        mime_desc.vf = ValOrFn::Function(Box::new(move || RefCell::borrow(&os_clone).read_mime()));

        let mut hash_desc = LocalDescBase::new(HASH_UUID, ver_flags);
        let os_clone = out_syncer.clone();
        hash_desc.vf = ValOrFn::Function(Box::new(move || RefCell::borrow(&os_clone).read_hash()));

        read_char.add_desc(ver_desc);
        //read_char.add_desc(loc_desc);
        read_char.add_desc(len_desc);
        read_char.add_desc(mime_desc);
        read_char.add_desc(hash_desc);

        copy_service.add_char(read_char);
        //permissions
        let mut write_flags = CharFlags::default();
        write_flags.secure_write = true;
        write_flags.encrypt_write = true;
        write_flags.write_wo_response = true;
        write_flags.encrypt_read = true;
        write_flags.notify = true;
        write_flags.indicate = true;
        let mut write_char = LocalCharBase::new(&write_uuid, write_flags);
        // setup write call back
        write_char.enable_write_fd(true);
        write_char.set_handle(handles.write);
        //let last_written = Rc::new(RefCell::new(Clip::default()));
        //let lw_clone = last_written.clone();
        // let (v, l) = syncer.read_fn();
        let os_clone = out_syncer.clone();
        let is_clone = in_syncer.clone();

        write_char.write_callback = Some(Box::new(move |bytes| {
            let (clip, val) = is_clone.borrow_mut().process_write(bytes);
            if verbose >= 2 {
                eprintln!("Received message: {:?}", bytes);
                eprintln!("write_char.write_callback(): replying with: {:?}", val);
            }
            if let Some(clip) = clip {
                update_clipboard(&clip).ok();
                println!("Updading clipboard with new remote clip: {:?}", clip);
                //lw_clone.replace(clip);
                os_clone.replace(OutSyncer::new(clip, verbose));
            }
            Ok((Some(ValOrFn::Value(val)), true))
        }));

        let mut ver_desc = LocalDescBase::new(&ver_uuid, ver_flags);
        ver_desc.vf = ValOrFn::Value([0, 0][..].into());
        ver_desc.set_handle(handles.write_ver);
        write_char.add_desc(ver_desc);
        copy_service.add_char(write_char);
        /*
        let mut write_serv = copy_service.get_char(&write_uuid);
        write_serv.write_val_or_fn(&mut ValOrFn::Value(v, l));*/

        blue.add_service(copy_service).unwrap();
        /*loop {
            blue.process_requests().unwrap();
        }*/
        let e = match blue.register_application() {
            Ok(_) => break (in_syncer, out_syncer),
            Err(e) => e,
        };
        if handles == Handles::default() {
            panic!("Failed to register_application with zeroed handles: {:?}", e);
        } else {
            eprintln!("Failed to register_application!: {:?}\nTrying getting new handles.", e);
            handles = Handles::default();
        }
    };

    let mut adv = Advertisement::new(AdType::Peripheral, name);
    adv.duration = 2;
    adv.timeout = std::u16::MAX;
    adv.service_uuids.push(serv_uuid.clone());
    blue.set_power(true)
        .expect("Failed to power on bluetooth controller!");
    blue.set_discoverable(true)
        .expect("Failed to make device discoverable!");
    let adv_idx = match blue.start_adv(adv) {
        Ok(idx) => idx,
        Err((idx, _)) => {
            eprintln!("Warning: failed to start advertisement");
            idx
        }
    };

    // Get new handles back
    let mut serv = blue.get_service(&serv_uuid).unwrap();
    let serv_handle = serv.handle();
    let mut read_char = serv.get_child(&read_uuid).unwrap();
    let read_ver = read_char.get_child(&ver_uuid).unwrap().handle();
    let read_handle = read_char.handle();
    let mut write_char = serv.get_child(&write_uuid).unwrap();
    let write_ver = write_char.get_child(&ver_uuid).unwrap().handle();
    let write_handle = write_char.handle();
    let new_handles = Handles {
        service: serv_handle,
        read: read_handle,
        read_ver,
        write: write_handle,
        write_ver,
    };

    if handles != new_handles {
        eprintln!("Handles changed: {:?}\nWriting back.", new_handles);
        if let Err(e) = set_handles::<&Path>(None, &new_handles) {
            eprintln!("Failed to writeback handles!: {:?}", e);
        }
    }

    let mut target = Instant::now();
    loop {
        // check for writes to local clipboard from GATT client
        let now = Instant::now();
        blue.process_requests().unwrap();
        let mut serv = blue.get_service(&serv_uuid).unwrap();
        let mut write_char = serv.get_child(&write_uuid).unwrap();
        write_char.check_write_fd();

        // check for the read characteristic
        let mut read_char = serv.get_child(&read_uuid).unwrap();
        read_char.check_write_fd();
        let mut os_bor = out_syncer.borrow_mut();
        if let Err(e) = os_bor.indicate_local(&mut read_char) {
            eprintln!("Error indicating: {:?}", e);
        }
        drop(os_bor);

        // check for local updates to clipboard;
        if let None = target.checked_duration_since(now) {
            target = now + Duration::from_secs(2);
            let new_clip = match get_clipboard() {
                Ok(o) => o,
                Err(e) => {
                    if verbose > 0 {
                        eprintln!("Failed to read clipboard: {:?}", e);
                    }
                    continue;
                }
            };
            if RefCell::borrow(&out_syncer).get_clip() != &*new_clip {
                println!("Clipboard changed, pushing changes: {:?}", new_clip);
                in_syncer.borrow_mut().update_with_local(new_clip.clone());
                out_syncer.replace(OutSyncer::new(new_clip, verbose));
            }
            match blue.restart_adv(adv_idx) {
                Ok(v) => {
                    if v {
                        if let Err(e) = blue.set_discoverable(true) {
                            eprintln!("Failed to set to discoverable: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    if verbose > 1 {
                        eprintln!("Failed to set to started advertisement: {:?}", e);
                    }
                }
            }
        }
        sleep((now + Duration::from_millis(200)).saturating_duration_since(Instant::now()));
    }
}

fn parser<'a, 'b>() -> App<'a, 'b> {
    App::new("Airboard Server")
        .version(VERSION)
        .author("Curtis Maves <curtis@maves.io>")
        .arg(
            Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .multiple(true),
        )
        .arg(
            Arg::with_name("no-filter")
                .short("n")
                .long("nofilter")
                .help("Allows all incoming Dbus messages."),
        )
        .arg(
            Arg::with_name("hostname")
                .short("h")
                .long("hostname")
                .value_name("NAME")
                .takes_value(true),
        )
}
