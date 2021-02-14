
use rustable::{Error as BLEError, MAX_APP_MTU, MAX_CHAR_LEN};
use rustable::gatt::{LocalChar, AttValue, ValOrFn};
use sha2::{Digest, Sha256};

use std::convert::TryInto;

const MIN_NOTIFY_LEN: usize = 64;

pub struct Clip {
    mime: String,
    hash: [u8; 32],
    data: Vec<u8>,
}
impl std::fmt::Debug for Clip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clip")
            .field("mime", &self.mime)
            .field("hash", &self.hash)
            .field("data.len()", &self.len())
            .finish()
    }
}

impl Clip {
    pub fn new(data: Vec<u8>, mime: String) -> Self {
        let hash = Sha256::digest(&data).into();
        Self { hash, data, mime }
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
    pub fn mime(&self) -> &str {
        &self.mime
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
}
impl Default for Clip {
    fn default() -> Self {
        Self::new(Vec::new(), "text/plain;charset=utf-8".to_owned())
    }
}
impl PartialEq<Clip> for Clip {
    fn eq(&self, other: &Clip) -> bool {
        if self.mime != other.mime {
                return false;
        }
        if self.data.len() != other.data.len() {
            return false;
        }
        self.hash == other.hash
    }
}

pub struct OutSyncer {
    clip: Clip,
    cur_pos: u32,
    written: u32,
    verbose: u8,
    notify_len: usize,
    notify_max: usize,
}

impl OutSyncer {
    pub fn new(clip: Clip, verbose: u8) -> Self {
        OutSyncer {
            clip,
            cur_pos: std::u32::MAX,
            written: 0,
            verbose,
            notify_max: MAX_APP_MTU,
            notify_len: MIN_NOTIFY_LEN,
        }
    }
	pub fn get_buf(&self) -> &[u8] {
		&self.clip.data()
	}
    pub fn get_clip(&self) -> &Clip {
        &self.clip
    }
    pub fn increment_notify_len(&mut self) -> usize {
        self.notify_len += 1;
        self.notify_len
    }
    pub fn notify_len(&self) -> usize {
        self.notify_len
    }
    pub fn reduce_notify_len(&mut self) -> Option<(usize, usize)> {
        if self.clip.len() as u32 == self.cur_pos || self.cur_pos == std::u32::MAX {
            let old_len = self.notify_len;
            self.notify_len = (self.notify_len * 7 / 8).max(MIN_NOTIFY_LEN);
            Some((old_len, self.notify_len))
        } else {
            None
        }
    }
    pub fn indicate_local(
        &mut self,
        local_char: &mut LocalChar,
    ) -> Result<(), BLEError> {
        if self.clip.len() as u32 == self.cur_pos {
            return Ok(());
        }
        if self.cur_pos == std::u32::MAX {
            let v = self.read_fn();
            let ret = local_char.notify(Some(&mut ValOrFn::Value(v)));
            return ret;
        }
        let max_out = self.notify_len * 1;
        let target = self.clip.len().min(self.cur_pos as usize + max_out);
        while (self.written as usize) < target {
            let v = self.generate_char(self.written, self.notify_len);
            let len = v.len() - 4;
            if self.verbose >= 2 {
                eprintln!("Indicating at position {}.", self.written);
            }
            if let Err(e) = local_char.notify(Some(&mut ValOrFn::Value(v))) {
                return Err(e);
            }
            self.written += len as u32;
        }
        Ok(())
    }
    fn generate_char(&self, loc: u32, max_len: usize) -> AttValue {
       let mut v = AttValue::default();
       v.extend_from_slice(&loc.to_be_bytes());
        if loc == std::u32::MAX {
            v.extend_from_slice(&self.clip.hash());
            v.extend_from_slice(&(self.clip.len() as u32).to_be_bytes());
            v.extend_from_slice(self.clip.mime().as_bytes());
        } else {
            let end = self.clip.len().min(loc as usize + max_len - 4);
            v.extend_from_slice(&self.get_buf()[loc as usize..end]);
        }
        v
    }
    pub fn read_fn(&self) -> AttValue {
        /* In theory the MAX_CHAR_LEN should work but android will only accept charactertiscs of len
           MAX_CHAR_LEN - 1. Not sure if this is a bug or the standard.
         */
        self.generate_char(self.cur_pos, MAX_CHAR_LEN - 1)

    }
    /*
    pub fn read_loc(&self) -> AttValue {
        if self.cur_pos == std::u32::MAX {
            AttValue::from(&[255, 255, 255, 255][..])
        } else {
            AttValue::from(&self.cur_pos.to_be_bytes()[..])
        }
    }
    */
    pub fn read_mime(&self) -> AttValue {
        AttValue::from(self.clip.mime().as_bytes())
    }
    pub fn read_len(&self) -> AttValue {
        let bytes = (self.clip.len() as u32).to_be_bytes();
        AttValue::from(&bytes[..])
    }
    pub fn read_hash(&self) -> AttValue {
        AttValue::from(&self.clip.hash()[..])
    }
    /// If this function returns `true` it indicates there is still data to be written to the client.
    pub fn update_pos(&mut self, data: &[u8]) {
        let mut int_buf = [0; 4];
        int_buf.copy_from_slice(&data[..4]);
        let cur_pos = u32::from_be_bytes(int_buf);
        if self.cur_pos == std::u32::MAX && cur_pos as usize <= self.clip.len() {
            if data.len() != 36 {
                return;
            }
            if &data[4..36] != self.clip.hash() {
                return;
            }
            self.cur_pos = cur_pos;
            self.written = cur_pos;
            return;
        }
        if cur_pos as usize > self.clip.len() {
            self.cur_pos = std::u32::MAX;
        } else {
            // self.dirty = cur_pos as usize != self.buf.len();
            self.cur_pos = if cur_pos == std::u32::MAX 
                || (data.len() == 36 && &data[4..36] != self.clip.hash()) 
           {
                // Client in waiting for new message or bad hash received
                std::u32::MAX
            } else {
                if cur_pos <= self.cur_pos {
                    // a duplicate ACK was received
                    self.written = cur_pos
                }
                self.written = self.written.max(cur_pos);
                cur_pos
            };
        }
    }
}



#[derive(Default)]
pub struct InSyncer {
    hash: [u8; 32],
    msg_length: usize,
    data_buf: Vec<u8>,
}

impl InSyncer {
    pub fn read_fn(&self) -> AttValue {
        let mut ret = AttValue::default();
        ret.extend_from_slice(&self.data_buf.len().to_be_bytes()[4..8]);
        ret.extend_from_slice(&self.msg_length.to_be_bytes()[4..8]);
        ret.extend_from_slice(&self.hash);
        return ret;
    }
    pub fn process_write(&mut self, v: &[u8]) -> Result<Option<Vec<u8>>, ()> {
        let s: &Self = self;
        if v.len() < 8 {
            return Err(());
        }
        let mut int_buf = [0; 4];
        int_buf.copy_from_slice(&v[..4]);
        let p_num = u32::from_be_bytes(int_buf);
        int_buf.copy_from_slice(&v[4..8]);
        let p_len = u32::from_be_bytes(int_buf) as usize;
        if p_num == std::u32::MAX {
            if v.len() != 40 {
                let s: &Self = self;
                return Err(());
            }
            self.data_buf.clear();
            self.msg_length = p_len;
            self.hash.copy_from_slice(&v[8..])
        } else {
            if p_len + 8 != v.len() {
                return Err(());
            }
            if self.data_buf.len() + p_len > self.msg_length {
                return Err(());
            }
            self.data_buf.extend_from_slice(&v[8..]);
        }
        if self.data_buf.len() == self.msg_length {
            let res = Sha256::digest(&self.data_buf);
            if res.as_slice() == self.hash {
                Ok(Some(self.data_buf.clone()))
            } else {
                self.data_buf.clear();
                Err(())
            }
        } else {
            Ok(None)
        }
    }
}
