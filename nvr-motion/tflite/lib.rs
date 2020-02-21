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

use std::convert::TryFrom;
use std::ffi::CStr;
use std::marker::PhantomData;
use std::os::raw::c_char;
use std::ptr;

pub static MODEL: &'static [u8] = include_bytes!("edgetpu.tflite");

pub static LABELS: [Option<&'static str>; 90] = [
    Some("person"),
    Some("bicycle"),
    Some("car"),
    Some("motorcycle"),
    Some("airplane"),
    Some("bus"),
    Some("train"),
    Some("truck"),
    Some("boat"),
    Some("traffic light"),
    Some("fire hydrant"),
    None,
    Some("stop sign"),
    Some("parking meter"),
    Some("bench"),
    Some("bird"),
    Some("cat"),
    Some("dog"),
    Some("horse"),
    Some("sheep"),
    Some("cow"),
    Some("elephant"),
    Some("bear"),
    Some("zebra"),
    Some("giraffe"),
    None,
    Some("backpack"),
    Some("umbrella"),
    None,
    None,
    Some("handbag"),
    Some("tie"),
    Some("suitcase"),
    Some("frisbee"),
    Some("skis"),
    Some("snowboard"),
    Some("sports ball"),
    Some("kite"),
    Some("baseball bat"),
    Some("baseball glove"),
    Some("skateboard"),
    Some("surfboard"),
    Some("tennis racket"),
    Some("bottle"),
    None,
    Some("wine glass"),
    Some("cup"),
    Some("fork"),
    Some("knife"),
    Some("spoon"),
    Some("bowl"),
    Some("banana"),
    Some("apple"),
    Some("sandwich"),
    Some("orange"),
    Some("broccoli"),
    Some("carrot"),
    Some("hot dog"),
    Some("pizza"),
    Some("donut"),
    Some("cake"),
    Some("chair"),
    Some("couch"),
    Some("potted plant"),
    Some("bed"),
    None,
    Some("dining table"),
    None,
    None,
    Some("toilet"),
    None,
    Some("tv"),
    Some("laptop"),
    Some("mouse"),
    Some("remote"),
    Some("keyboard"),
    Some("cell phone"),
    Some("microwave"),
    Some("oven"),
    Some("toaster"),
    Some("sink"),
    Some("refrigerator"),
    None,
    Some("book"),
    Some("clock"),
    Some("vase"),
    Some("scissors"),
    Some("teddy bear"),
    Some("hair drier"),
    Some("toothbrush"),
];

pub mod edgetpu;

enum TfLiteDelegate {}
enum TfLiteInterpreter {}
enum TfLiteInterpreterOptions {}
enum TfLiteModel {}
pub enum Tensor {}  // aka TfLiteTensor

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(C)]
pub enum Type {  // aka TfLiteType
  NoType = 0,
  Float32 = 1,
  Int32 = 2,
  UInt8 = 3,
  Int64 = 4,
  String = 5,
  Bool = 6,
  Int16 = 7,
  Complex64 = 8,
  Int8 = 9,
  Float16 = 10,
}

#[derive(Copy, Clone)]
#[repr(C)]
struct TfLiteStatus(libc::c_int);

// #[link(name = "tensorflowlite_c")
extern "C" {
    fn TfLiteModelCreate(model_data: *const u8, model_size: usize) -> *mut TfLiteModel;
    fn TfLiteModelDelete(model: *mut TfLiteModel);

    fn TfLiteInterpreterOptionsCreate() -> *mut TfLiteInterpreterOptions;
    fn TfLiteInterpreterOptionsDelete(interpreter: *mut TfLiteInterpreterOptions);
    fn TfLiteInterpreterOptionsAddDelegate(options: *mut TfLiteInterpreterOptions,
                                           delegate: *mut TfLiteDelegate);

    fn TfLiteInterpreterCreate(model: *const TfLiteModel, options: *const TfLiteInterpreterOptions)
                               -> *mut TfLiteInterpreter;
    fn TfLiteInterpreterDelete(interpreter: *mut TfLiteInterpreter);
    fn TfLiteInterpreterAllocateTensors(interpreter: *mut TfLiteInterpreter) -> TfLiteStatus;
    fn TfLiteInterpreterGetInputTensorCount(interpreter: *const TfLiteInterpreter) -> libc::c_int;
    fn TfLiteInterpreterGetInputTensor(interpreter: *const TfLiteInterpreter, input_index: i32)
                                       -> *mut Tensor;
    fn TfLiteInterpreterInvoke(interpreter: *mut TfLiteInterpreter) -> TfLiteStatus;
    fn TfLiteInterpreterGetOutputTensorCount(interpreter: *const TfLiteInterpreter) -> libc::c_int;
    fn TfLiteInterpreterGetOutputTensor(interpreter: *const TfLiteInterpreter, output_index: i32)
                                       -> *const Tensor;

    fn TfLiteTensorType(tensor: *const Tensor) -> Type;
    fn TfLiteTensorNumDims(tensor: *const Tensor) -> i32;
    fn TfLiteTensorDim(tensor: *const Tensor, dim_index: i32) -> i32;
    fn TfLiteTensorByteSize(tensor: *const Tensor) -> usize;
    fn TfLiteTensorData(tensor: *const Tensor) -> *mut u8;
    fn TfLiteTensorName(tensor: *const Tensor) -> *const c_char;

    fn TfLiteTypeGetName(type_: Type) -> *const c_char;
}

impl TfLiteStatus {
    fn to_result(self) -> Result<(), ()> {
        match self.0 {
            0 => Ok(()),
            _ => Err(()),
        }
    }
}

pub struct InterpreterBuilder<'a> {
    options: ptr::NonNull<TfLiteInterpreterOptions>,
    _delegate_refs: PhantomData<&'a ()>,
}

impl<'a> InterpreterBuilder<'a> {
    pub fn new() -> Self {
        Self {
            options: ptr::NonNull::new(unsafe { TfLiteInterpreterOptionsCreate() }).unwrap(),
            _delegate_refs: PhantomData,
        }
    }

    pub fn add_delegate(&mut self, d: &'a Delegate) {
        unsafe { TfLiteInterpreterOptionsAddDelegate(self.options.as_ptr(), d.delegate.as_ptr()) }
    }

    pub fn build(self, model: &Model) -> Result<Interpreter<'a>, ()> {
        let interpreter = unsafe {
            TfLiteInterpreterCreate(model.0.as_ptr(), self.options.as_ptr())
        };
        let interpreter = Interpreter {
            interpreter: ptr::NonNull::new(interpreter).ok_or(())?,
            _delegate_refs: PhantomData,
        };
        unsafe { TfLiteInterpreterAllocateTensors(interpreter.interpreter.as_ptr()) }.to_result()?;
        Ok(interpreter)
    }
}

impl<'a> Drop for InterpreterBuilder<'a> {
    fn drop(&mut self) {
        unsafe { TfLiteInterpreterOptionsDelete(self.options.as_ptr()) };
    }
}

pub struct Interpreter<'a> {
    interpreter: ptr::NonNull<TfLiteInterpreter>,
    _delegate_refs: PhantomData<&'a ()>,
}

impl<'a> Interpreter<'a> {
    pub fn builder() -> InterpreterBuilder<'a> { InterpreterBuilder::new() }

    pub fn invoke(&mut self) -> Result<(), ()> {
        unsafe { TfLiteInterpreterInvoke(self.interpreter.as_ptr()) }.to_result()
    }

    pub fn inputs(&mut self) -> InputTensors {
        let len = usize::try_from(unsafe {
            TfLiteInterpreterGetInputTensorCount(self.interpreter.as_ptr())
        }).unwrap();
        InputTensors {
            interpreter: self,
            len,
        }
    }

    pub fn outputs(&self) -> OutputTensors {
        let len = usize::try_from(unsafe {
            TfLiteInterpreterGetOutputTensorCount(self.interpreter.as_ptr())
        }).unwrap();
        OutputTensors {
            interpreter: self,
            len,
        }
    }
}

impl<'a> Drop for Interpreter<'a> {
    fn drop(&mut self) {
        unsafe { TfLiteInterpreterDelete(self.interpreter.as_ptr()) };
    }
}

pub struct InputTensors<'i> {
    interpreter: &'i Interpreter<'i>,
    len: usize,
}

impl<'i> InputTensors<'i> {
    pub fn len(&self) -> usize { self.len }
}

impl<'i> std::ops::Index<usize> for InputTensors<'i> {
    type Output = Tensor;

    fn index(&self, index: usize) -> &Tensor {
        let index = i32::try_from(index).unwrap();
        unsafe { &*TfLiteInterpreterGetInputTensor(self.interpreter.interpreter.as_ptr(), index) }
    }
}

impl<'i> std::ops::IndexMut<usize> for InputTensors<'i> {
    fn index_mut(&mut self, index: usize) -> &mut Tensor {
        let index = i32::try_from(index).unwrap();
        unsafe {
            &mut *TfLiteInterpreterGetInputTensor(self.interpreter.interpreter.as_ptr(), index)
        }
    }
}

pub struct OutputTensors<'i> {
    interpreter: &'i Interpreter<'i>,
    len: usize,
}

impl<'i> OutputTensors<'i> {
    pub fn len(&self) -> usize { self.len }
}

impl<'i> std::ops::Index<usize> for OutputTensors<'i> {
    type Output = Tensor;

    fn index(&self, index: usize) -> &Tensor {
        let index = i32::try_from(index).unwrap();
        unsafe { &*TfLiteInterpreterGetOutputTensor(self.interpreter.interpreter.as_ptr(), index) }
    }
}

impl Tensor {
    pub fn type_(&self) -> Type { unsafe { TfLiteTensorType(self) } }
    pub fn num_dims(&self) -> usize {
        usize::try_from(unsafe { TfLiteTensorNumDims(self) }).unwrap()
    }
    pub fn dim(&self, i: usize) -> usize {
        assert!(i < self.num_dims());
        let i = i32::try_from(i).unwrap();
        usize::try_from(unsafe { TfLiteTensorDim(self, i) }).unwrap()
    }
    pub fn byte_size(&self) -> usize {
        unsafe { TfLiteTensorByteSize(self) }
    }
    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(TfLiteTensorData(self), self.byte_size()) }
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(TfLiteTensorData(self), self.byte_size()) }
    }
    pub fn f32s(&self) -> &[f32] {
        // Tensors are aligned.
        assert_eq!(self.type_(), Type::Float32);
        let bytes = self.bytes();
        unsafe {
            std::slice::from_raw_parts(bytes.as_ptr() as *const f32, bytes.len() >> 2)
        }
    }
    pub fn name(&self) -> &str {
        unsafe { CStr::from_ptr(TfLiteTensorName(self)) }.to_str().unwrap()
    }
}

impl std::fmt::Debug for Tensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dims = self.num_dims();
        let mut first = true;
        write!(f, "{}: ", self.name())?;
        for i in 0..dims {
            if !first {
                f.write_str("x")?;
            }
            first = false;
            write!(f, "{}", self.dim(i))?;
        }
        write!(f, " {:?}", self.type_())
    }
}

impl std::fmt::Debug for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(unsafe { CStr::from_ptr(TfLiteTypeGetName(*self)) }.to_str().unwrap())
    }
}

pub struct Delegate {
    delegate: ptr::NonNull<TfLiteDelegate>,
    free: unsafe extern "C" fn(*mut TfLiteDelegate),
}

impl Drop for Delegate {
    fn drop(&mut self) {
        unsafe { (self.free)(self.delegate.as_ptr()) };
    }
}

pub struct Model(ptr::NonNull<TfLiteModel>);

impl Model {
    pub fn from_static(model: &'static [u8]) -> Result<Self, ()> {
        let m = unsafe { TfLiteModelCreate(model.as_ptr(), model.len()) };
        Ok(Model(ptr::NonNull::new(m).ok_or(())?))
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        unsafe { TfLiteModelDelete(self.0.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn create_drop_model() {
        let _m = super::Model::from_static(super::MODEL).unwrap();
    }

    #[test]
    fn lifecycle() {
        let m = super::Model::from_static(super::MODEL).unwrap();
        let delegate;
        let mut builder = super::Interpreter::builder();
        let devices = super::edgetpu::Devices::list();
        if !devices.is_empty() {
            delegate = devices[0].create_delegate().unwrap();
            builder.add_delegate(&delegate);
        }
        let mut interpreter = builder.build(&m).unwrap();
        println!("interpreter with {} inputs, {} outputs",
                 interpreter.inputs().len(), interpreter.outputs().len());
        let inputs = interpreter.inputs();
        for i in 0 .. inputs.len() {
            println!("input: {:?}", inputs[i]);
        }
        let outputs = interpreter.outputs();
        for i in 0 .. outputs.len() {
            println!("output: {:?}", outputs[i]);
        }
    }
}
