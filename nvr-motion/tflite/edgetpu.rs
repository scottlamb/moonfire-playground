// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 Scott Lamb <slamb@slamb.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::ptr;

// Matches edgetpu_c.h: enum edgetpu_device_type
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub enum Type {
    ApexPci = 0,
    ApexUsb = 1,
}

// Matches edgetpu_c.h:struct edgetpu_device
#[repr(C)]
pub struct Device {
    type_: Type,
    path: *const c_char,
}

// matches edgetpu_c.h:struct edgetpu_option
#[repr(C)]
struct RawOption {
    name: *const c_char,
    value: *const c_char,
}

// #[link(name = "edgetpu")
extern "C" {
    fn edgetpu_list_devices(num_devices: *mut usize) -> *mut Device;
    fn edgetpu_free_devices(dev: *mut Device);
    fn edgetpu_create_delegate(type_: Type, name: *const libc::c_char,
                               options: *const RawOption, num_options: usize)
                               -> *mut super::TfLiteDelegate;
    fn edgetpu_free_delegate(delegate: *mut super::TfLiteDelegate);
    fn edgetpu_verbosity(verbosity: libc::c_int);
    fn edgetpu_version() -> *const c_char;
}

pub fn version() -> &'static str {
    unsafe { CStr::from_ptr(edgetpu_version()) }.to_str().unwrap()
}

pub fn verbosity(verbosity: libc::c_int) { unsafe { edgetpu_verbosity(verbosity) }; }

pub struct Devices {
    devices: ptr::NonNull<Device>,
    num_devices: usize,
}

impl Devices {
    pub fn list() -> Self {
        let mut num_devices = 0usize;
        let ptr = unsafe { edgetpu_list_devices(&mut num_devices) };
        let devices = match num_devices {
            0 => ptr::NonNull::dangling(),
            _ => ptr::NonNull::new(ptr).unwrap(),
        };
        Devices {
            devices,
            num_devices,
        }
    }

    pub fn is_empty(&self) -> bool { self.num_devices == 0 }
    pub fn len(&self) -> usize { self.num_devices }
}

impl std::ops::Deref for Devices {
    type Target = [Device];

    fn deref(&self) -> &[Device] {
        unsafe { std::slice::from_raw_parts(self.devices.as_ptr(), self.num_devices) }
    }
}

impl<'a> std::iter::IntoIterator for &'a Devices {
    type Item = &'a Device;
    type IntoIter = std::slice::Iter<'a, Device>;

    fn into_iter(self) -> std::slice::Iter<'a, Device> { self.iter() }
}

impl Drop for Devices {
    fn drop(&mut self) {
        if self.num_devices > 0 {
            unsafe { edgetpu_free_devices(self.devices.as_ptr()) };
        }
    }
}

impl Device {
    pub fn create_delegate(&self) -> Result<super::Delegate, ()> {
        // TODO: support options?
        let delegate = unsafe { edgetpu_create_delegate(self.type_, self.path, ptr::null(), 0) };
        let delegate = ptr::NonNull::new(delegate).ok_or(())?;
        Ok(super::Delegate {
            delegate,
            free: edgetpu_free_delegate,
        })
    }

    pub fn type_(&self) -> Type { self.type_ }
    pub fn path(&self) -> &CStr { unsafe { CStr::from_ptr(self.path) } }
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}@{}", self.type_, self.path().to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version() {
        println!("edgetpu version: {}", super::version());
    }

    #[test]
    fn list_devices() {
        let devices = super::Devices::list();
        println!("{} edge tpu devices:", devices.len());
        for d in &devices {
            println!("device: {:?}", d);
        }
    }

    #[test]
    fn create_delegate() {
        let devices = super::Devices::list();
        if !devices.is_empty() {
            devices[0].create_delegate().unwrap();
        }
    }
}
