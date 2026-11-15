//! Compatibility surface for the frame hand-off buffer.

pub use crate::pipeline::{
    BufferError, BufferRead, BufferedFrame, EndSend, FrameBufferConfig, FrameConsumer, FrameMeta,
    FrameProducer, FrameSend, MAX_BUFFER_DURATION_MS, MIN_BUFFER_DURATION_MS, frame_buffer,
};
