use proto::inferencer_server::{Inferencer, InferencerServer};
use std::collections::HashMap;
use std::convert::TryFrom;
use tokio::sync::Mutex;

type BoxedError = Box<dyn std::error::Error + 'static>;

mod proto {
    //include!(concat!(env!("OUT_DIR"), "/org.moonfire_nvr.inferencer.rs"));
    tonic::include_proto!("org.moonfire_nvr.inferencer");
}

//const MODEL_UUID: Uuid = Uuid::from_u128(0x4d1c73aa_b6ef_4986_a01d_3abe94693c4c);

struct MyInferencer {
    interpreter: Mutex<moonfire_tflite::Interpreter<'static>>,
    model: proto::Model,
}

impl MyInferencer {
    fn new(delegate: Option<&'static moonfire_tflite::Delegate>)
           -> Result<Self, BoxedError> {
        let m = moonfire_tflite::Model::from_static(moonfire_tflite::MODEL).unwrap();
        let mut builder = moonfire_tflite::Interpreter::builder();
        if let Some(d) = delegate {
            builder.add_delegate(d);
        }
        let interpreter = builder.build(&m).unwrap();

        let mut model = proto::Model::default();
        model.uuid = "4d1c73aa-b6ef-4986-a01d-3abe94693c4c".to_owned();
        model.r#type = proto::ModelType::ModelObjectDetection as i32;
        model.active = true;
        model.input_parameters = Some(proto::ImageParameters {
            pixel_format: proto::PixelFormat::Rgb24 as i32,
            width: 300,
            height: 300,
        });
        model.labels = HashMap::new();
        for (i, l) in moonfire_tflite::LABELS.iter().enumerate() {
            if let Some(l) = l {
                model.labels.insert(u32::try_from(i).unwrap(), l.to_string());
            }
        }

        Ok(Self {
            interpreter: Mutex::new(interpreter),
            model,
        })
    }
}

/// Processes a single prescaled, raw image.
fn process_image(interpreter: &mut moonfire_tflite::Interpreter, model: &proto::Model, image: &[u8])
                 -> Result<proto::ImageResult, tonic::Status> {
    if model.r#type != proto::ModelType::ModelObjectDetection as i32 {
        return Err(tonic::Status::new(tonic::Code::Unimplemented, format!("only object detection models are supported, not {}", model.r#type)));
    }
    {
        let mut inputs = interpreter.inputs();
        if inputs.len() != 1 {
            return Err(tonic::Status::new(tonic::Code::Internal,
                                          format!("expected model to have 1 input; has {}",
                                                  inputs.len())));
        }
        let input = &mut inputs[0];
        if input.byte_size() != image.len() {
            return Err(tonic::Status::new(tonic::Code::InvalidArgument,
                                          format!("expected {}-byte input; got {}-byte input",
                                                  input.byte_size(), image.len())));
        }
        input.bytes_mut().copy_from_slice(&image[..]);
    }
    interpreter.invoke()
        .map_err(|()| tonic::Status::new(tonic::Code::Unknown, "interpreter failed"))?;
    let outputs = interpreter.outputs();
    if outputs.len() != 4 {
        return Err(tonic::Status::new(tonic::Code::Internal,
                                      format!("expected model to have 4 outputs; has {}",
                                              outputs.len())));
    }
    let boxes = outputs[0].f32s();
    let labels = outputs[1].f32s();
    let scores = outputs[2].f32s();
    let mut r = proto::ObjectDetectionResult::default();
    for (i, &score) in scores.iter().enumerate() {
        if score <= 0. {
            continue;
        }
        let label = labels[i];
        if !(0. <= label && label <= u32::max_value() as f32) {
            continue;
        }
        let y = boxes[4*i + 0];
        let x = boxes[4*i + 1];
        let h = boxes[4*i + 2] - y;
        let w = boxes[4*i + 3] - x;
        r.x.push(x);
        r.y.push(y);
        r.h.push(h);
        r.w.push(w);
        r.score.push(scores[i]);
        r.label.push(label as u32);
    }
    Ok(proto::ImageResult {
        model_result: Some(proto::image_result::ModelResult::ObjectDetectionResult(r)),
    })
}

#[tonic::async_trait]
impl Inferencer for MyInferencer {
    async fn list_models(
        &self,
        _request: tonic::Request<proto::ListModelsRequest>,
    ) -> Result<tonic::Response<proto::ListModelsResponse>, tonic::Status> {
        let resp = proto::ListModelsResponse {
            model: vec![self.model.clone()],
        };
        Ok(tonic::Response::new(resp))
    }

    async fn process_image(
        &self,
        request: tonic::Request<proto::ProcessImageRequest>,
    ) -> Result<tonic::Response<proto::ProcessImageResponse>, tonic::Status> {
        let request = request.into_inner();
        if request.model_uuid != self.model.uuid {
            return Err(tonic::Status::new(tonic::Code::FailedPrecondition,
                                          format!("active model is {}, not {}",
                                                  &self.model.uuid, &request.model_uuid)));
        }

        let mut interpreter = self.interpreter.lock().await;
        let result = process_image(&mut interpreter, &self.model, &request.image[..])?;
        Ok(tonic::Response::new(proto::ProcessImageResponse {
            result: Some(result),
        }))
    }

    async fn process_video(
        &self,
        _request: tonic::Request<tonic::Streaming<proto::ProcessVideoRequest>>,
    ) -> Result<tonic::Response<proto::ProcessVideoResponse>, tonic::Status> {
        Err(tonic::Status::new(tonic::Code::Unimplemented, "process_video unimplemented"))
    }
}

#[tokio::main]
async fn main() -> Result<(), BoxedError> {
    let addr = "0.0.0.0:8085".parse()?;

    let devices = moonfire_tflite::edgetpu::Devices::list();
    let delegate = if devices.is_empty() {
        None
    } else {
        Some(&*Box::leak(Box::new(devices[0].create_delegate().unwrap())))
    };
    let inferencer = MyInferencer::new(delegate)?;

    tonic::transport::Server::builder()
        .add_service(InferencerServer::new(inferencer))
        .serve(addr)
        .await?;

    Ok(())
}
