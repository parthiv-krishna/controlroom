//! Declarative macros that define an API's method table from one list, so method names, marker
//! types, params and result types, the typed request enum and the TypeScript method map can't
//! drift apart.

/// Defines request methods. For each entry `Marker = "name" (Params) -> Result;` it generates:
///
/// - a zero-sized `Marker` type implementing [`crate::jsonrpc::Method`];
/// - a variant `Marker(Params)` of the request enum, serialized as `{"method": "name", "params": ...}`;
/// - an entry in the enum's `METHODS` list and in the TypeScript method map.
macro_rules! rpc_methods {
    (
        $(#[doc = $enum_doc:literal])*
        pub enum $Enum:ident;
        $(
            $(#[doc = $doc:literal])*
            $Marker:ident = $name:literal ($Params:ty) -> $Result:ty;
        )*
    ) => {
        $(
            $(#[doc = $doc])*
            #[doc = ""]
            #[doc = concat!("Method `", $name, "`.")]
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            pub struct $Marker;

            impl $crate::jsonrpc::Method for $Marker {
                const NAME: &'static str = $name;
                type Params = $Params;
                type Result = $Result;
            }
        )*

        $(#[doc = $enum_doc])*
        #[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize, ::ts_rs::TS)]
        #[serde(tag = "method", content = "params")]
        pub enum $Enum {
            $(
                $(#[doc = $doc])*
                #[serde(rename = $name)]
                $Marker($Params),
            )*
        }

        impl $Enum {
            /// Every method name in this API, in declaration order.
            pub const METHODS: &'static [&'static str] = &[$($name),*];

            /// The method name of this call.
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$Marker(_) => $name,)*
                }
            }

            /// Decodes a call from a request's `method` and `params`. Unknown methods give
            /// `method_not_found`; bad params give `invalid_params`.
            pub fn from_parts(
                method: &str,
                params: ::serde_json::Value,
            ) -> Result<Self, $crate::error::RpcError> {
                match method {
                    $($name => Ok(Self::$Marker($crate::jsonrpc::parse_params($name, params)?)),)*
                    other => Err($crate::error::RpcError::new(
                        $crate::error::ErrorData::MethodNotFound,
                        format!("unknown method {other:?}"),
                    )),
                }
            }

            /// Decodes a call from an untyped request.
            pub fn from_request(
                request: $crate::jsonrpc::RpcRequest<::serde_json::Value>,
            ) -> Result<Self, $crate::error::RpcError> {
                Self::from_parts(&request.method, request.params)
            }

            /// Method table for TypeScript generation.
            pub(crate) fn specs() -> Vec<$crate::typescript::MethodSpec> {
                vec![$(
                    $crate::typescript::MethodSpec::request::<$Marker>(&[$($doc),*]),
                )*]
            }
        }
    };
}

/// Defines notifications. For each entry `Marker = "name" (Params);` it generates a zero-sized
/// `Marker` implementing [`crate::jsonrpc::NotificationMethod`] and a variant of the notification
/// enum, serialized as `{"method": "name", "params": ...}`.
macro_rules! rpc_notifications {
    (
        $(#[doc = $enum_doc:literal])*
        pub enum $Enum:ident;
        $(
            $(#[doc = $doc:literal])*
            $Marker:ident = $name:literal ($Params:ty);
        )*
    ) => {
        $(
            $(#[doc = $doc])*
            #[doc = ""]
            #[doc = concat!("Notification `", $name, "`.")]
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            pub struct $Marker;

            impl $crate::jsonrpc::NotificationMethod for $Marker {
                const NAME: &'static str = $name;
                type Params = $Params;
            }
        )*

        $(#[doc = $enum_doc])*
        #[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize, ::ts_rs::TS)]
        #[serde(tag = "method", content = "params")]
        pub enum $Enum {
            $(
                $(#[doc = $doc])*
                #[serde(rename = $name)]
                $Marker($Params),
            )*
        }

        impl $Enum {
            /// Every notification name in this API, in declaration order.
            pub const NOTIFICATIONS: &'static [&'static str] = &[$($name),*];

            /// The method name of this notification.
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$Marker(_) => $name,)*
                }
            }

            /// Decodes a notification from its `method` and `params`. A receiver drops (and
            /// logs) notifications it can't decode; they never close the connection.
            pub fn from_parts(
                method: &str,
                params: ::serde_json::Value,
            ) -> Result<Self, $crate::error::RpcError> {
                match method {
                    $($name => Ok(Self::$Marker($crate::jsonrpc::parse_params($name, params)?)),)*
                    other => Err($crate::error::RpcError::new(
                        $crate::error::ErrorData::MethodNotFound,
                        format!("unknown notification {other:?}"),
                    )),
                }
            }

            /// Notification table for TypeScript generation.
            pub(crate) fn specs() -> Vec<$crate::typescript::MethodSpec> {
                vec![$(
                    $crate::typescript::MethodSpec::notification::<$Marker>(&[$($doc),*]),
                )*]
            }
        }
    };
}
