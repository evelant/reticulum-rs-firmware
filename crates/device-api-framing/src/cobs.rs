//! Small, bounded COBS codec used only by the record owner.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EncodeError {
    OutputTooSmall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecodeError {
    ZeroCode,
    TruncatedBlock,
    OutputTooSmall,
}

pub(crate) const fn maximum_encoded_length(decoded_length: usize) -> usize {
    decoded_length + (decoded_length / 254) + 1
}

pub(crate) fn encode(input: &[u8], output: &mut [u8]) -> Result<usize, EncodeError> {
    if output.is_empty() {
        return Err(EncodeError::OutputTooSmall);
    }

    let mut read = 0;
    let mut write = 1;
    let mut code_index = 0;
    let mut code = 1_u8;

    while read < input.len() {
        let byte = input[read];
        read += 1;

        if byte == 0 {
            output[code_index] = code;
            code_index = write;
            write = write.checked_add(1).ok_or(EncodeError::OutputTooSmall)?;
            if write > output.len() {
                return Err(EncodeError::OutputTooSmall);
            }
            code = 1;
            continue;
        }

        if write >= output.len() {
            return Err(EncodeError::OutputTooSmall);
        }
        output[write] = byte;
        write += 1;
        code = code.wrapping_add(1);

        if code == u8::MAX {
            output[code_index] = code;
            code_index = write;
            write = write.checked_add(1).ok_or(EncodeError::OutputTooSmall)?;
            if write > output.len() {
                return Err(EncodeError::OutputTooSmall);
            }
            code = 1;
        }
    }

    output[code_index] = code;
    Ok(write)
}

pub(crate) fn decode(input: &[u8], output: &mut [u8]) -> Result<usize, DecodeError> {
    let mut read: usize = 0;
    let mut write: usize = 0;

    while read < input.len() {
        let code = input[read];
        if code == 0 {
            return Err(DecodeError::ZeroCode);
        }
        read += 1;

        let block_length = usize::from(code) - 1;
        let block_end = read
            .checked_add(block_length)
            .ok_or(DecodeError::TruncatedBlock)?;
        if block_end > input.len() {
            return Err(DecodeError::TruncatedBlock);
        }
        let output_end = write
            .checked_add(block_length)
            .ok_or(DecodeError::OutputTooSmall)?;
        if output_end > output.len() {
            return Err(DecodeError::OutputTooSmall);
        }
        output[write..output_end].copy_from_slice(&input[read..block_end]);
        read = block_end;
        write = output_end;

        if code != u8::MAX && read < input.len() {
            if write >= output.len() {
                return Err(DecodeError::OutputTooSmall);
            }
            output[write] = 0;
            write += 1;
        }
    }

    Ok(write)
}
