mod bindings {
    windows::include_bindings!();
}

use std::ffi::c_void;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::ptr::null_mut;
use std::slice::from_raw_parts;
use libc::{free, malloc, wchar_t, wcslen};

use crate::{Error, NetworkInterface, NetworkInterfaceConfig, Result};

use self::bindings::Windows::Win32;
use self::Win32::Networking::WinSock::{SOCKADDR_IN, SOCKADDR_IN6};
use self::Win32::NetworkManagement::IpHelper::{
    ADDRESS_FAMILY, AF_UNSPEC, IP_ADAPTER_ADDRESSES_LH, GetAdaptersAddresses,
};

/// An alias for `IP_ADAPTER_ADDRESSES_LH`
type AdapterAddress = IP_ADAPTER_ADDRESSES_LH;

/// An alias for `GET_ADAPTERS_ADDRESSES_FLAGS`
type GetAdaptersAddressesFlags =
    self::Win32::NetworkManagement::IpHelper::GET_ADAPTERS_ADDRESSES_FLAGS;

/// The buffer size indicated by the `SizePointer` parameter is too small to hold the
/// adapter information or the `AdapterAddresses` parameter is `NULL`. The `SizePointer`
/// parameter returned points to the required size of the buffer to hold the adapter
/// information.
///
/// Source: https://docs.microsoft.com/en-us/windows/win32/api/iphlpapi/nf-iphlpapi-getadaptersaddresses#return-value
const ERROR_BUFFER_OVERFLOW: u32 = 111;

/// Max tries allowed to call `GetAdaptersAddresses` on a loop basis
const MAX_TRIES: usize = 3;

/// Success execution output from `GetAdaptersAddresses` call
const GET_ADAPTERS_ADDRESSES_SUCCESS_RESULT: u32 = 0;

/// A constant to store `Win32::NetworkManagement::IpHelper::AF_INET.0` casted as `u16`
const AF_INET: u16 = self::Win32::NetworkManagement::IpHelper::AF_INET.0 as u16;

/// A constant to store `Win32::NetworkManagement::IpHelper::AF_INET6.0` casted as `u16`
const AF_INET6: u16 = self::Win32::NetworkManagement::IpHelper::AF_INET6.0 as u16;

/// The address family of the addresses to retrieve. This parameter must be one of the following values.
/// The default address family is `AF_UNSPECT` in order to gather both IPv4 and IPv6 network interfaces.
///
/// Source: https://docs.microsoft.com/en-us/windows/win32/api/iphlpapi/nf-iphlpapi-getadaptersaddresses#parameters
const GET_ADAPTERS_ADDRESSES_FAMILY: ADDRESS_FAMILY = ADDRESS_FAMILY(AF_UNSPEC.0);

const GET_ADAPTERS_ADDRESSES_FLAGS: GetAdaptersAddressesFlags =
    self::Win32::NetworkManagement::IpHelper::GET_ADAPTERS_ADDRESSES_FLAGS(0x0);

impl NetworkInterfaceConfig for NetworkInterface {
    fn show() -> Result<Vec<NetworkInterface>> {
        // Allocate a 15 KB buffer to start with.
        let mut size_pointer: u32 = 15000;
        let mut adapter_address = unsafe { malloc(size_pointer as usize) as *mut AdapterAddress };
        let mut iterations = 0;
        let mut get_adapter_addresses_result = 0;
        let mut network_interfaces: Vec<NetworkInterface> = Vec::new();

        while get_adapter_addresses_result != ERROR_BUFFER_OVERFLOW || iterations <= MAX_TRIES {
            adapter_address = unsafe { malloc(size_pointer as usize) as *mut AdapterAddress };

            if adapter_address.is_null() {
                // Memory allocation failed for IP_ADAPTER_ADDRESSES struct
                return Err(Error::GetIfAddrsError(
                    String::from("GetAdaptersAddresses"),
                    1,
                ));
            }

            get_adapter_addresses_result = unsafe {
                GetAdaptersAddresses(
                    GET_ADAPTERS_ADDRESSES_FAMILY,
                    GET_ADAPTERS_ADDRESSES_FLAGS,
                    null_mut::<c_void>(),
                    adapter_address,
                    &mut size_pointer,
                )
            };

            if get_adapter_addresses_result == ERROR_BUFFER_OVERFLOW {
                unsafe {
                    free(adapter_address as *mut c_void);
                };
                adapter_address = null_mut();
            } else {
                break;
            }

            iterations += 1;
        }

        if get_adapter_addresses_result == GET_ADAPTERS_ADDRESSES_SUCCESS_RESULT {
            let mut current_adapter_address = adapter_address;

            while !current_adapter_address.is_null() {
                let address_name = make_adapter_address_name(&current_adapter_address)?;
                let mut current_unicast_address = unsafe { (*adapter_address).FirstUnicastAddress };

                while !current_unicast_address.is_null() {
                    let address = unsafe { (*current_unicast_address).Address };

                    match unsafe { (*address.lpSockaddr).sa_family } {
                        AF_INET => {
                            let sockaddr = address.lpSockaddr as *mut SOCKADDR_IN;
                            let addr = make_ipv4_addr(&sockaddr)?;
                            let network_interface =
                                NetworkInterface::new_afinet(&address_name, addr, addr, None);

                            network_interfaces.push(network_interface);
                        }
                        AF_INET6 => {
                            let sockaddr = address.lpSockaddr as *mut SOCKADDR_IN6;
                            let addr = make_ipv6_addr(&sockaddr)?;
                            let network_interface =
                                NetworkInterface::new_afinet6(&address_name, addr, addr, None);

                            network_interfaces.push(network_interface);
                        }
                        _ => {}
                    }

                    if !current_unicast_address.is_null() {
                        current_unicast_address = unsafe { (*current_unicast_address).Next };
                    }
                }

                if !current_adapter_address.is_null() {
                    current_adapter_address = unsafe { (*current_adapter_address).Next };
                }
            }
        }

        Ok(network_interfaces)
    }
}

/// Retrieves the network interface name
fn make_adapter_address_name(adapter_address: &*mut AdapterAddress) -> Result<String> {
    let address_name = unsafe { (*(*adapter_address)).FriendlyName.0 };
    let address_name_length = unsafe { wcslen(address_name as *const wchar_t) };
    let byte_slice = unsafe { from_raw_parts(address_name, address_name_length) };
    let string = String::from_utf16(byte_slice).map_err(Error::from)?;

    Ok(string)
}

/// Creates a `Ipv6Addr` from a `SOCKADDR_IN6`
fn make_ipv6_addr(sockaddr: &*mut SOCKADDR_IN6) -> Result<Ipv6Addr> {
    let address_bytes = unsafe { (*(*sockaddr)).sin6_addr.u.Byte };
    let ip = Ipv6Addr::from(address_bytes);

    Ok(ip)
}

/// Creates a `Ipv4Addr` from a `SOCKADDR_IN`
fn make_ipv4_addr(sockaddr: &*mut SOCKADDR_IN) -> Result<Ipv4Addr> {
    let address = unsafe { (*(*sockaddr)).sin_addr.S_un.S_addr };

    if cfg!(target_endian = "little") {
        // due to a difference on how bytes are arranged on a
        // single word of memory by the CPU, swap bytes based
        // on CPU endianess to avoid having twisted IP addresses
        //
        // refer: https://github.com/rust-lang/rust/issues/48819
        return Ok(Ipv4Addr::from(address.swap_bytes()));
    }

    Ok(Ipv4Addr::from(address))
}

#[cfg(target_os = "windows")]
mod tests {
    #[test]
    fn show_network_interfaces() {
        use super::{NetworkInterface, NetworkInterfaceConfig};

        let network_interfaces = NetworkInterface::show().unwrap();

        assert!(network_interfaces.len() > 1);
    }
}