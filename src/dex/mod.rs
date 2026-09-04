//! DEX file parser — resolves the main Application class and Activity name
//! from the APK's `AndroidManifest.xml` and `classes.dex`.

/// Minimal binary XML (AXML) parser to extract the `android:name` attribute
/// from `<application>` and `<activity android:name=...>` tags.
/// Android's binary XML is a compact format with string pool + chunk records.
pub struct ManifestInfo {
    /// e.g. "jp.naver.line.android.LineLauncher"
    pub main_activity: Option<String>,
    /// e.g. "jp.naver.line.android.App"
    pub application_class: Option<String>,
    /// e.g. "jp.naver.line.android"
    pub package_name: Option<String>,
}

#[derive(Debug)]
pub struct DexInfo {
    pub class_count: usize,
    pub classes: Vec<String>,
    pub activities: Vec<String>,
    pub services: Vec<String>,
}

/// Parse a `classes.dex` file and extract class definitions.
pub fn parse_dex(data: &[u8]) -> anyhow::Result<DexInfo> {
    let dex_file = dex::DexReader::from_vec(data.to_vec())
        .map_err(|e| anyhow::anyhow!("DEX parse error: {:?}", e))?;

    let mut classes: Vec<String> = Vec::new();
    let mut activities: Vec<String> = Vec::new();
    let mut services: Vec<String> = Vec::new();

    for class in dex_file.classes() {
        if let Ok(c) = class {
            let jtype = c.jtype().to_string();
            classes.push(jtype.clone());
            if jtype.ends_with("Activity;") || jtype.contains("activity") {
                activities.push(jtype.clone());
            } else if jtype.ends_with("Service;") || jtype.contains("service") {
                services.push(jtype.clone());
            }
        }
    }

    let count = classes.len();
    Ok(DexInfo {
        class_count: count,
        classes,
        activities,
        services,
    })
}

impl ManifestInfo {
    /// Parse binary `AndroidManifest.xml` and extract key attributes.
    pub fn parse(data: &[u8]) -> Self {
        let string_pool = parse_axml_string_pool(data);
        let mut main_activity = None;
        let mut application_class = None;
        let mut package_name = None;

        // Simple scan: find START_ELEMENT chunks and extract attributes.
        // AXML chunk types: FILE_HEADER=0x0003, STRING_POOL=0x001, START_ELEMENT=0x0102
        let mut pos = 8usize; // skip file header
        while pos + 8 <= data.len() {
            let chunk_type = u16::from_le_bytes(data[pos..pos+2].try_into().unwrap_or_default());
            let _header_size = u16::from_le_bytes(data[pos+2..pos+4].try_into().unwrap_or_default()) as usize;
            let chunk_size = u32::from_le_bytes(data[pos+4..pos+8].try_into().unwrap_or_default()) as usize;
            if chunk_size == 0 { break; }

            if chunk_type == 0x0102 {
                // START_ELEMENT: header(16) + ns(4) + name(4) + attr_start(4)+attr_size(4)+attr_count(2)+...
                if pos + 32 > data.len() { pos += chunk_size; continue; }
                let elem_name_idx = u32::from_le_bytes(data[pos+20..pos+24].try_into().unwrap_or_default()) as usize;
                let attr_count = u16::from_le_bytes(data[pos+28..pos+30].try_into().unwrap_or_default()) as usize;
                let attrs_start = pos + 36;

                let elem_name = string_pool.get(elem_name_idx).cloned().unwrap_or_default();

                // Each attr = 5 × u32 = 20 bytes: ns, name, raw_val, value_type_size, value_data
                for i in 0..attr_count {
                    let a = attrs_start + i * 20;
                    if a + 20 > data.len() { break; }
                    let attr_name_idx = u32::from_le_bytes(data[a+4..a+8].try_into().unwrap_or_default()) as usize;
                    let attr_val_idx = u32::from_le_bytes(data[a+8..a+12].try_into().unwrap_or_default()) as usize;

                    let attr_name = string_pool.get(attr_name_idx).cloned().unwrap_or_default();
                    let attr_val  = string_pool.get(attr_val_idx).cloned().unwrap_or_default();

                    match (elem_name.as_str(), attr_name.as_str()) {
                        ("manifest", "package") => {
                            package_name = Some(attr_val);
                        }
                        ("application", "name") => {
                            application_class = Some(normalize_class_name(&attr_val));
                        }
                        ("activity", "name") if main_activity.is_none() => {
                            // First <activity> is typically the launcher
                            main_activity = Some(normalize_class_name(&attr_val));
                        }
                        _ => {}
                    }
                }
            }

            pos += chunk_size;
        }

        ManifestInfo { main_activity, application_class, package_name }
    }
}

fn normalize_class_name(name: &str) -> String {
    // Android allows relative class names starting with '.' (relative to package).
    // We'll store them as-is; caller can prefix with package name.
    name.to_string()
}

/// Parse the string pool from a binary AXML buffer.
/// Returns a Vec of UTF-8 strings indexed by string pool index.
fn parse_axml_string_pool(data: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    if data.len() < 8 { return strings; }

    // Find STRING_POOL chunk (type 0x0001)
    let mut pos = 8usize;
    while pos + 8 <= data.len() {
        let chunk_type = u16::from_le_bytes(data[pos..pos+2].try_into().unwrap_or_default());
        let _header_size = u16::from_le_bytes(data[pos+2..pos+4].try_into().unwrap_or_default()) as usize;
        let chunk_size = u32::from_le_bytes(data[pos+4..pos+8].try_into().unwrap_or_default()) as usize;
        if chunk_size == 0 { break; }

        if chunk_type == 0x0001 {
            // String pool header: string_count(4), style_count(4), flags(4), strings_start(4), styles_start(4)
            if pos + 28 > data.len() { break; }
            let string_count = u32::from_le_bytes(data[pos+8..pos+12].try_into().unwrap_or_default()) as usize;
            let flags = u32::from_le_bytes(data[pos+16..pos+20].try_into().unwrap_or_default());
            let strings_start = u32::from_le_bytes(data[pos+20..pos+24].try_into().unwrap_or_default()) as usize;
            let is_utf8 = flags & (1 << 8) != 0;

            let offsets_start = pos + 28;
            let str_data_start = pos + strings_start;

            for i in 0..string_count {
                let off_pos = offsets_start + i * 4;
                if off_pos + 4 > data.len() { break; }
                let str_off = u32::from_le_bytes(data[off_pos..off_pos+4].try_into().unwrap_or_default()) as usize;
                let str_start = str_data_start + str_off;

                let s = if is_utf8 {
                    parse_utf8_string(data, str_start)
                } else {
                    parse_utf16_string(data, str_start)
                };
                strings.push(s);
            }
            break;
        }

        pos += chunk_size;
    }
    strings
}

fn parse_utf8_string(data: &[u8], pos: usize) -> String {
    if pos + 2 > data.len() { return String::new(); }
    // Skip char length byte and byte length
    let _char_len = data[pos] as usize;
    let byte_pos = if data[pos] & 0x80 != 0 { pos + 2 } else { pos + 1 };
    let _byte_len = data[byte_pos] as usize;
    let str_pos = if data[byte_pos] & 0x80 != 0 { byte_pos + 2 } else { byte_pos + 1 };

    let end = data[str_pos..].iter().position(|&b| b == 0).unwrap_or(0);
    String::from_utf8_lossy(&data[str_pos..str_pos + end]).to_string()
}

fn parse_utf16_string(data: &[u8], pos: usize) -> String {
    if pos + 2 > data.len() { return String::new(); }
    let char_count = u16::from_le_bytes(data[pos..pos+2].try_into().unwrap_or_default()) as usize;
    let str_pos = pos + 2;
    if str_pos + char_count * 2 > data.len() { return String::new(); }
    let chars: Vec<u16> = (0..char_count)
        .map(|i| u16::from_le_bytes(data[str_pos + i*2..str_pos + i*2 + 2].try_into().unwrap_or_default()))
        .collect();
    String::from_utf16_lossy(&chars).to_string()
}
