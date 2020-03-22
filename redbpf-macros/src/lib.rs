// Copyright 2019 Authors of Red Sift
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

/*!
Procedural macros to help writing eBPF programs using the `redbpf-probes`
crate.

# Overview

`redbpf-macros` is part of the `redbpf` project. Together with
[`redbpf-probes`](https://ingraind.org/api/redbpf_probes/), it
provides an idiomatic Rust API to write programs that can be compiled to eBPF
bytecode and executed by the linux in-kernel eBPF virtual machine.

To streamline the process of working with eBPF programs even further,
`redbpf` also provides
[`cargo-bpf`](https://ingraind.org/api/cargo_bpf/) - a cargo subcommand
to simplify creating and building eBPF programs.

# Example

```no_run
#![no_std]
#![no_main]
use redbpf_probes::xdp::prelude::*;

// configure kernel version compatibility and license
program!(0xFFFFFFFE, "GPL");

#[xdp]
fn example_xdp_probe(ctx: XdpContext) -> XdpResult {

    // do something here

    Ok(XdpAction::Pass)
}
```
*/
extern crate proc_macro;
extern crate proc_macro2;
use proc_macro::TokenStream;
use proc_macro2::{Ident, Span, TokenStream as TokenStream2};
use quote::quote;
use std::str;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{parse_macro_input, parse_quote, parse_str, Expr, ExprLit, File, ItemFn, Lit, Result};

fn inline_string_literal(e: &Expr) -> (TokenStream2, TokenStream2) {
    let bytes = match e {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => s.value().into_bytes(),
        _ => panic!("expected string literal"),
    };

    inline_bytes(bytes)
}

fn inline_bytes(mut bytes: Vec<u8>) -> (TokenStream2, TokenStream2) {
    bytes.push(0u8);
    let len = bytes.len();
    let bytes = bytes;
    let ty = quote!([u8; #len]);
    let array_lit = quote!([#(#bytes),*]);

    (ty, array_lit)
}

struct Args(Punctuated<Expr, Comma>);

impl Parse for Args {
    fn parse(input: ParseStream) -> Result<Args> {
        Ok(Args(Punctuated::parse_terminated(input)?))
    }
}

/// Generates program metadata.
///
/// Takes two arguments, the `LINUX_VERSION_CODE` the program is compatible with,
/// and the license. The special version code `0xFFFFFFFE` can be used to signify
/// any kernel version.
///
/// # Example
///
/// ```no_run
/// # #![no_std]
/// # #![no_main]
/// # use redbpf_macros::program;
/// program!(0xFFFFFFFE, "GPL");
/// # fn main() {}
/// ```
///
#[proc_macro]
pub fn program(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Args);
    let mut args = input.0.iter();
    let version = args.next().expect("no version");
    let license = args.next().expect("no license");
    let (license_ty, license) = inline_string_literal(&license);
    let (panic_ty, panic_msg) = inline_bytes(b"panic".to_vec());
    let mut tokens = quote! {
        #[no_mangle]
        #[link_section = "license"]
        pub static _license: #license_ty = #license;

        #[no_mangle]
        #[link_section = "version"]
        pub static _version: u32 = #version;

        #[panic_handler]
        #[no_mangle]
        pub extern "C" fn rust_begin_panic(info: &::core::panic::PanicInfo) -> ! {
            use ::redbpf_probes::helpers::{bpf_trace_printk};

            let msg: #panic_ty = #panic_msg;
            bpf_trace_printk(&msg);

            unsafe { core::hint::unreachable_unchecked() }
        }
    };

    let mem = str::from_utf8(include_bytes!("mem.rs")).unwrap();
    let mem: File = parse_str(&mem).unwrap();
    tokens.extend(quote! {
        #mem
    });

    tokens.into()
}

#[doc(hidden)]
#[proc_macro]
pub fn impl_network_buffer_array(_: TokenStream) -> TokenStream {
    let mut tokens = TokenStream2::new();
    for i in 1..=512usize {
        tokens.extend(quote! {
            impl NetworkBufferArray for [u8; #i] {}
        });
    }

    tokens.into()
}

/// Attribute macro that must be used when creating [eBPF
/// maps](https://ingraind.org/api/redbpf_probes/maps/index.html).
///
/// # Example
/// ```no_run
/// # use redbpf_probes::kprobe::prelude::*;
/// #[map("dns_queries")]
/// static mut queries: PerfMap<Query> = PerfMap::with_max_entries(1024);
///
/// struct Query {
/// // ...
/// }
/// ```
#[proc_macro_attribute]
pub fn map(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attrs as Expr);
    let name = match attrs {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => s.value(),
        _ => panic!("expected string literal"),
    };

    let section_name = format!("maps/{}", name);
    let item = TokenStream2::from(item);
    let tokens = quote! {
        #[no_mangle]
        #[link_section = #section_name]
        #item
    };

    tokens.into()
}

fn probe_impl(ty: &str, attrs: TokenStream, item: ItemFn, mut name: String) -> TokenStream {
    if !attrs.is_empty() {
        name = match parse_macro_input!(attrs as Expr) {
            Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) => s.value(),
            _ => panic!("expected string literal"),
        }
    };

    let section_name = format!("{}/{}", ty, name);
    let tokens = quote! {
        #[no_mangle]
        #[link_section = #section_name]
        #item
    };

    tokens.into()
}

fn wrap_kprobe(item: ItemFn) -> ItemFn {
    let ident = item.sig.ident.clone();
    let outer_ident = Ident::new(&format!("outer_{}", ident), Span::call_site());
    parse_quote! {
        fn #outer_ident(ctx: *mut c_void) -> i32 {
            let regs = ::redbpf_probes::kprobe::Registers::from(ctx);
            let _ = #ident(regs);
            return 0;

            #item
        }
    }
}

/// Attribute macro that must be used to define [`kprobes`](https://www.kernel.org/doc/Documentation/kprobes.txt).
///
/// # Example
/// ```no_run
/// use redbpf_probes::kprobe::prelude::*;
///
/// #[kprobe("__x64_sys_clone")]
/// fn clone_enter(regs: Registers) {
///     // this is executed when clone() is invoked
/// }
/// ```
#[proc_macro_attribute]
pub fn kprobe(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    let name = item.sig.ident.to_string();
    let wrapper = wrap_kprobe(item);
    probe_impl("kprobe", attrs, wrapper, name)
}

/// Attribute macro that must be used to define [`kretprobes`](https://www.kernel.org/doc/Documentation/kprobes.txt).
///
/// # Example
/// ```no_run
/// use redbpf_probes::kprobe::prelude::*;
///
/// #[kretprobe("__x64_sys_clone")]
/// fn clone_exit(regs: Registers) {
///     // this is executed when clone() returns
/// }
/// ```
#[proc_macro_attribute]
pub fn kretprobe(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    let name = item.sig.ident.to_string();
    let wrapper = wrap_kprobe(item);
    probe_impl("kretprobe", attrs, wrapper, name)
}

/// Attribute macro that must be used to define [`uprobes`](https://www.kernel.org/doc/Documentation/trace/uprobetracer.txt).
///
/// # Example
/// ```no_run
/// use redbpf_probes::uprobe::prelude::*;
///
/// #[uprobe]
/// fn getaddrinfo(regs: Registers) {
///     // this is executed when getaddrinfo() is invoked
/// }
/// ```
#[proc_macro_attribute]
pub fn uprobe(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    let name = item.sig.ident.to_string();
    let wrapper = wrap_kprobe(item);
    probe_impl("uprobe", attrs, wrapper, name)
}

/// Attribute macro that must be used to define [`uretprobes`](https://www.kernel.org/doc/Documentation/trace/uprobetracer.txt).
///
/// # Example
/// ```no_run
/// use redbpf_probes::uprobe::prelude::*;
///
/// #[uretprobe]
/// fn getaddrinfo(regs: Registers) {
///     // this is executed when getaddrinfo() returns
/// }
/// ```
#[proc_macro_attribute]
pub fn uretprobe(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    let name = item.sig.ident.to_string();
    let wrapper = wrap_kprobe(item);
    probe_impl("uretprobe", attrs, wrapper, name)
}

/// Attribute macro that must be used to define [`XDP` probes](https://www.iovisor.org/technology/xdp).
///
/// See also the [`XDP` API provided by
/// `redbpf-probes`](https://ingraind.org/api/redbpf_probes/xdp/index.html).
///
/// # Example
/// ```no_run
/// use redbpf_probes::xdp::prelude::*;
///
/// #[xdp]
/// fn probe(ctx: XdpContext) -> XdpResult {
///     // do something with the packet
///
///     Ok(XdpAction::Pass)
/// }
/// ```
#[proc_macro_attribute]
pub fn xdp(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    let name = item.sig.ident.to_string();
    let ident = item.sig.ident.clone();
    let outer_ident = Ident::new(&format!("outer_{}", ident), Span::call_site());
    let wrapper = parse_quote! {
        fn #outer_ident(ctx: *mut ::redbpf_probes::bindings::xdp_md) -> ::redbpf_probes::xdp::XdpAction {
            let ctx = ::redbpf_probes::xdp::XdpContext { ctx };
            return match #ident(ctx) {
                Ok(action) => action,
                Err(_) => ::redbpf_probes::xdp::XdpAction::Pass
            };

            #item
        }
    };
    probe_impl("xdp", attrs, wrapper, name)
}

/// Attribute macro that must be used to define [`socket
/// filter`](https://www.kernel.org/doc/Documentation/networking/filter.txt)
/// probes.
///
/// See also the [`socket filter` API provided by
/// `redbpf-probes`](https://ingraind.org/api/redbpf_probes/socket_filter/index.html).
///
/// # Example
/// ```no_run
/// use redbpf_probes::socket_filter::prelude::*;
///
/// #[socket_filter]
/// fn probe(skb: SkBuff) -> SkBuffResult {
///     Ok(SkBuffAction::SendToUserspace)
/// }
/// ```
#[proc_macro_attribute]
pub fn socket_filter(attrs: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    let name = item.sig.ident.to_string();
    let ident = item.sig.ident.clone();
    let outer_ident = Ident::new(&format!("outer_{}", ident), Span::call_site());
    let wrapper = parse_quote! {
        fn #outer_ident(skb: *const ::redbpf_probes::bindings::__sk_buff) -> i32 {
            let skb = ::redbpf_probes::socket_filter::SkBuff { skb };
            return match #ident(skb) {
                Ok(::redbpf_probes::socket_filter::SkBuffAction::SendToUserspace) => -1,
                _ => 0
            };

            #item
        }
    };

    probe_impl("socketfilter", attrs, wrapper, name)
}
