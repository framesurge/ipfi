use crate::error::Error;
use serde::{de::DeserializeOwned, Serialize};

/// A trait that represents the arguments to a remote procedure. This is predominantly for
/// convenience, and largely from [here](https://docs.rs/rhai/latest/src/rhai/func/args.rs.html#14-60).
pub trait ProcedureArgs {
    /// Transforms the arguments into a flattened byte array, ready for transmission. At the other end, this
    /// should be able to be deserialized as a tuple, once a MessagePack length prefix is prepended.
    fn into_bytes(self) -> Result<Vec<u8>, Error>;
}

impl<T: Serialize> ProcedureArgs for Vec<T> {
    fn into_bytes(self) -> Result<Vec<u8>, Error> {
        let mut data = Vec::new();
        for elem in self {
            data.extend(rmp_serde::to_vec(&elem)?);
        }

        Ok(data)
    }
}

impl<T: Serialize, const N: usize> ProcedureArgs for [T; N] {
    #[inline]
    fn into_bytes(self) -> Result<Vec<u8>, Error> {
        let mut data = Vec::new();
        for elem in self {
            data.extend(rmp_serde::to_vec(&elem)?);
        }

        Ok(data)
    }
}

/// Macro to implement [`ProcedureArgs`] for tuples of standard types (each can be converted into bytes).
macro_rules! impl_args {
    ($($p:ident),*) => {
        impl<$($p: Serialize),*> ProcedureArgs for ($($p,)*)
        {
            #[inline]
            fn into_bytes(self) -> Result<Vec<u8>, Error> {
                #[allow(non_snake_case)]
                let ($($p,)*) = self;
                #[allow(unused_mut)]
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

/// An internal trait used to get the lengths of tuples. This is needed so we can insert the correct
/// MessagePack length marker when we deserialize the arguments provied piecemeal over an interface.
pub trait Tuple {
    fn len() -> u32;
}
macro_rules! impl_tuple {
    ($($p:ident),*) => {
        impl<$($p: Serialize),*> Tuple for ($($p,)*)
        {
            #[inline]
            fn len() -> u32 {
                #[allow(unused_mut)]
                let mut counter = 0;
                $(
                    nothing::<$p>();
                    counter += 1;
                )*
                counter
            }
        }

        impl_tuple!(@pop $($p),*);
    };
    (@pop) => {
    };
    (@pop $head:ident) => {
        impl_tuple!();
    };
    (@pop $head:ident $(, $tail:ident)+) => {
        impl_tuple!($($tail),*);
    };
}
impl_tuple!(A, B, C, D, E, F, G, H, J, K, L, M, N, P, Q, R, S, T, U, V);

// Used to keep track of type parameters in macro expansions
fn nothing<T>() {}
