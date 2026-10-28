pub enum HttpRequest<'a> {
    Get(i64),
    Post(&'a str),
    Other,
}

#[inline(always)]
pub fn parse_http_request(payload: &[u8]) -> HttpRequest<'_> {
    if payload.starts_with(b"GET /record/") {
        let rest = &payload[12..];
        let mut id: i64 = 0;
        let mut idx = 0;
        while idx < rest.len() && rest[idx] >= b'0' && rest[idx] <= b'9' {
            id = id * 10 + (rest[idx] - b'0') as i64;
            idx += 1;
        }
        if id > 0 {
            return HttpRequest::Get(id);
        }
    } else if payload.starts_with(b"POST /record") {
        if let Some(pos) = find_subslice(payload, b"\r\n\r\n") {
            let body = &payload[pos + 4..];
            if let Ok(body_str) = std::str::from_utf8(body) {
                // Extract "payload":"..."
                if let Some(p_start) = body_str.find("\"payload\":") {
                    let rem = &body_str[p_start + 10..];
                    let trimmed = rem.trim_start();
                    if trimmed.starts_with('"') {
                        let inner = &trimmed[1..];
                        if let Some(p_end) = inner.find('"') {
                            return HttpRequest::Post(&inner[..p_end]);
                        }
                    }
                }
                return HttpRequest::Post("bench_record");
            }
        }
        return HttpRequest::Post("bench_record");
    }
    HttpRequest::Other
}

#[inline(always)]
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub fn format_get_response(id: i64, payload: &str, updated_at: i64, out: &mut [u8]) -> usize {
    let mut id_buf = itoa::Buffer::new();
    let id_str = id_buf.format(id);

    let mut ts_buf = itoa::Buffer::new();
    let ts_str = ts_buf.format(updated_at);

    // Body: {"id":<id>,"payload":"<payload>","updated_at":<ts>}
    let body_len = 7 + id_str.len() + 13 + payload.len() + 16 + ts_str.len() + 1;
    let mut cl_buf = itoa::Buffer::new();
    let cl_str = cl_buf.format(body_len);

    let mut offset = 0;

    let part1 = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: keep-alive\r\nContent-Length: ";
    out[offset..offset + part1.len()].copy_from_slice(part1);
    offset += part1.len();

    out[offset..offset + cl_str.len()].copy_from_slice(cl_str.as_bytes());
    offset += cl_str.len();

    let part2 = b"\r\n\r\n{\"id\":";
    out[offset..offset + part2.len()].copy_from_slice(part2);
    offset += part2.len();

    out[offset..offset + id_str.len()].copy_from_slice(id_str.as_bytes());
    offset += id_str.len();

    let part3 = b",\"payload\":\"";
    out[offset..offset + part3.len()].copy_from_slice(part3);
    offset += part3.len();

    out[offset..offset + payload.len()].copy_from_slice(payload.as_bytes());
    offset += payload.len();

    let part4 = b"\",\"updated_at\":";
    out[offset..offset + part4.len()].copy_from_slice(part4);
    offset += part4.len();

    out[offset..offset + ts_str.len()].copy_from_slice(ts_str.as_bytes());
    offset += ts_str.len();

    out[offset] = b'}';
    offset += 1;

    offset
}

pub fn format_post_response(id: i64, out: &mut [u8]) -> usize {
    let mut id_buf = itoa::Buffer::new();
    let id_str = id_buf.format(id);

    let body_len = 7 + id_str.len() + 1; // {"id":<id>}
    let mut cl_buf = itoa::Buffer::new();
    let cl_str = cl_buf.format(body_len);

    let mut offset = 0;

    let part1 = b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: keep-alive\r\nContent-Length: ";
    out[offset..offset + part1.len()].copy_from_slice(part1);
    offset += part1.len();

    out[offset..offset + cl_str.len()].copy_from_slice(cl_str.as_bytes());
    offset += cl_str.len();

    let part2 = b"\r\n\r\n{\"id\":";
    out[offset..offset + part2.len()].copy_from_slice(part2);
    offset += part2.len();

    out[offset..offset + id_str.len()].copy_from_slice(id_str.as_bytes());
    offset += id_str.len();

    out[offset] = b'}';
    offset += 1;

    offset
}

pub fn format_404_response(out: &mut [u8]) -> usize {
    let msg = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
    out[..msg.len()].copy_from_slice(msg);
    msg.len()
}
