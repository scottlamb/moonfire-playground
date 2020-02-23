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

fn main() {
    println!("cargo:rustc-link-search=native=/home/slamb/git/tensorflow/bazel-bin/tensorflow/lite/c");
    println!("cargo:rustc-link-lib=tensorflowlite_c");
    println!("cargo:rustc-link-lib=edgetpu");
    //let mut wrapper = cc::Build::new();
    //wrapper.include("/home/slamb/git/tensorflow");
    //wrapper.include("/home/slamb/git/tensorflow/bazel-bin/tensorflow/tools/pip_package/build_pip_package.runfiles/com_google_absl");
    //wrapper.include("/home/slamb/git/tensorflow/bazel-bin/external/flatbuffers/_virtual_includes/flatbuffers");
    //wrapper.file("wrapper.cc").compile("libwrapper.a");
}
