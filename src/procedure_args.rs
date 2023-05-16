use crate::error::Error;
use serde::{Serialize, de::DeserializeOwned};

/// A trait that represents the arguments to a remote procedure. This is predominantly for
/// convenience, and largely from [here](https://docs.rs/rhai/latest/src/rhai/func/args.rs.html#14-60).
pub trait ProcedureArgs {
    /// Transforms the arguments into a flattened byte array, ready for transmission. At the other end, this
    /// should be able to be deserialized as a tuple, once a MessagePack length prefix is prepended.
    fn into_bytes(self) -> Result<Vec<u8>, Error>;
}

impl<T: Serialize + DeserializeOwned> ProcedureArgs for Vec<T> {
    fn into_bytes(self) -> Result<Vec<u8>, Error> {
        let mut data = Vec::new();
        for elem in self {
            data.extend(rmp_serde::to_vec(&elem)?);
        }

        Ok(data)
    }
}

impl<T: Serialize + DeserializeOwned, const N: usize> ProcedureArgs for [T; N] {
    #[inline]
    fn into_bytes(self) -> Result<Vec<u8>, Error> {
        let mut data = Vec::new();
        for elem in self {
            data.extend(rmp_serde::to_vec(&elem)?);
        }
        // let mut vec = Vec::new();
        // // This lets us reinterpret as a tuple later in deserialization
        // rmp::encode::write_array_len(&mut vec, self.len() as u32);
        // vec.extend(data);

        Ok(data)
    }
}

/// Macro to implement [`ProcedureArgs`] for tuples of standard types (each can be converted into bytes).
macro_rules! impl_args {
    ($($p:ident),*) => {
        impl<$($p: Serialize + DeserializeOwned),*> ProcedureArgs for ($($p,)*)
        {
            #[inline]
            #[allow(unused_variables)]
            fn into_bytes(self) -> Result<Vec<u8>, Error> {
                let ($($p,)*) = self;
                let mut data: Vec<u8> = Vec::new();
                $(data.extend(rmp_serde::to_vec(&$p)?);)*

                Ok(data)
            }
        }

        impl_args!(@pop $($p),*);
    };
    (@pop) => {
    };
    (@pop $head:ident) => {
        impl_args!();
    };
    (@pop $head:ident $(, $tail:ident)+) => {
        impl_args!($($tail),*);
    };
}

impl_args!(A, B, C, D, E, F, G, H, J, K, L, M, N, P, Q, R, S, T, U, V);
