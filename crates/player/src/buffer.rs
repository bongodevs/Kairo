//! Compatibility surface for the frame hand-off buffer.

pub use crate::pipeline::{
    BufferError, BufferRead, BufferedFrame, EndSend, FrameBufferConfig, FrameConsumer, FrameFormat,
    FrameMeta, FrameProducer, FrameSend, MAX_BUFFER_DURATION_MS, MAX_FRAME_PAYLOAD_BYTES,
    MAX_PCM_S16_BYTES, MIN_BUFFER_DURATION_MS, frame_buffer,
};
