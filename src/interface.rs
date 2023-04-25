use std::{sync::{RwLock, Arc}, mem::MaybeUninit};
use serde::de::DeserializeOwned;

/// An inter-process communication (IPC) interface based on the arbitrary reception of bytes.
///
/// This is formed in a message-based interface, with messages identified by 32-bit integers. Each time
/// a new value is sent into the interface, it will be added to the provided message, allowing data from
/// multiple sources to be simultaneously accumulated. Once `-1` is sent, the message will be marked as
/// completed, and future attempts to write to it will fail. Thread-safety and mutability locks are
/// maintained independently for every message we store.
///
/// Note that this system is perfectly thread-safe, and it is perfectly valid for multiple data sources
/// to send multiple messages simultaneously, while multiple receivers simultaneously wait for multiple
/// messages. Once a message is accumulated, it will be stored in the buffer until the interface is
/// dropped.
pub struct Interface<const MESSAGES: usize> {
    messages: [Arc<RwLock<(Vec<u8>, bool)>>; MESSAGES],
}
impl<const MESSAGES: usize> Interface<MESSAGES> {
    /// Initializes a new interface with some other host module.
    pub fn new() -> Self {
        // SAFETY: We're dealing with an array of `MaybeUninit`s, which don't need initialization
        let mut arr: [MaybeUninit<Arc<RwLock<(Vec<u8>, bool)>>>; MESSAGES] = unsafe {
            MaybeUninit::uninit().assume_init()
        };
        for elem in &mut arr[..] {
            // If this panics because we've run out of memory (not impossible if we allocate too much space for messages), we'll
            // get a memory leak on some `MaybeUninit`s, but they conveniently don't actually mean anything, so there's no memory
            // safety issue!
            elem.write(Arc::new(RwLock::new((Vec::new(), false))));
        }
        let messages = unsafe { std::mem::transmute_copy::<_, [Arc<RwLock<(Vec<u8>, bool)>>; MESSAGES]>(&arr) };

        Self {
            messages,
        }
    }
    /// Sends the given element through to the interface, adding it to the byte array of the message with
    /// the given 32-bit identifier. If `-1` is provided, the message will be marked as completed. If the
    /// message was already marked as completed, no matter what the input, this will return `false`, indicating
    /// a failure. Likewise, if the given message is out-of-bounds for this interface, `false` will be
    /// returned.
    ///
    /// Ths reason this fails gracefully for an out-of-bounds message is so other programs, which may not have
    /// been involved in deciding how many message buffers should be in this interface, can make mistakes without
    /// blowing up the program they're sending data. Callers shoudl be careful, however, to inform the sender that
    /// they are sending data to the void.
    pub fn send(&self, datum: i8, message: usize) -> bool {
        let message = match self.messages.get(message) {
            Some(msg) => msg,
            None => return false
        };

        let mut message = message.write().unwrap();
        if message.1 {
            false
        } else if datum < 0 {
            message.1 = true;
            true
        } else {
            message.0.push(datum as u8);
            true
        }
    }
    /// Sends many bytes through to the interface. When you have many bytes instead of just one at a time, this
    /// method should be preferred. Note that this method will not allow the termination of a message, and that should
    /// be handled separately.
    ///
    /// This will return `false` if the given message index was out-of-bounds, or if it has already been terminated.
    pub fn send_many(&self, data: &[u8], message: usize) -> bool {
        let message = match self.messages.get(message) {
            Some(msg) => msg,
            None => return false
        };

        let mut message = message.write().unwrap();
        if message.1 {
            false
        } else {
            message.0.extend(data);
            true
        }
    }
    /// Explicitly terminates the message with the given index. This will return `true` if it was possible, or `false`
    /// if the message had already been terminated (or if it is out-of-bounds).
    pub fn terminate_message(&self, message: usize) -> bool {
        let message = match self.messages.get(message) {
            Some(msg) => msg,
            None => return false
        };

        let mut message = message.write().unwrap();
        if message.1 {
            false
        } else {
            message.1 = true;
            true
        }
    }
    /// Gets an object of the given type from the module interface. This will get from the earliest completed message,
    /// blocking if there are only incomplete messages available until one is completed. Depending on the caller's behavior,
    /// this may block forever if they do not terminate a message.
    pub fn get<T: DeserializeOwned>(&self, message: usize) -> Result<T, rmp_serde::decode::Error> {
        let message = self.get_raw(message);
        rmp_serde::decode::from_slice(&message)
    }
    /// Same as `.get()`, but gets the raw byte array instead. Note that this will copy the underlying bytes to return
    /// them outside the thread-lock.
    ///
    /// # Panics
    ///
    /// This will panic if the given message index is out-of-bounds for this interface. Since the caller instantiates
    /// this interface with its message size, they should check this before calling this method.
    pub fn get_raw(&self, message: usize) -> Vec<u8> {
        let message = match self.messages.get(message) {
            Some(msg) => msg,
            // Clearer error than a simple out-of-bounds
            None => panic!("attempted to get message at index '{}' from interface with max index '{}'", message, MESSAGES - 1)
        };

        // Wait until this message is completed
        while !message.read().unwrap().1 {
            // Don't just loop on every tick, give the system some time to receive data
            std::thread::yield_now();
        }

        message.read().unwrap().0.to_vec()
    }
}
