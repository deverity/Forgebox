//! One typed method per API method, generated from the method table so that the client
//! cannot drift from the contracts.

use super::{Client, ClientError, Subscription};
use crate::api::methods::api_methods;
#[allow(unused_imports)]
use crate::api::*;

macro_rules! client_method {
    (call $name:literal $fn:ident $params:ident $res:ident) => {
        #[doc = concat!("Calls `", $name, "`.")]
        pub async fn $fn(&self, params: $params) -> Result<$res, ClientError> {
            self.call::<crate::api::methods::$fn>(params).await
        }
    };
    (job $name:literal $fn:ident $params:ident $res:ident) => {
        #[doc = concat!(
                    "Starts `", $name, "` (long-running): answers with the job at once; the job ",
                    "result is `", stringify!($res), "`."
                )]
        pub async fn $fn(&self, params: $params) -> Result<JobDto, ClientError> {
            self.call::<crate::api::methods::$fn>(params).await
        }
    };
    (stream $name:literal $fn:ident $params:ident $res:ident) => {
        #[doc = concat!("Subscribes with `", $name, "` (resumes after reconnections).")]
        pub async fn $fn(&self, params: $params) -> Result<Subscription<$res>, ClientError> {
            self.subscribe::<crate::api::methods::$fn>(params).await
        }
    };
}

macro_rules! client_methods {
    ($($kind:ident $name:literal $fn:ident ($params:ident) -> $res:ident [$($flags:tt)*] $cli:literal $(=> $tpl:literal)?;)*) => {
        /// Typed methods, one per row of the method table.
        impl Client {
            $(client_method!($kind $name $fn $params $res);)*
        }
    };
}

api_methods!(client_methods);
