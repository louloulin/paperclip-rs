//! 归档导入用例的 zip 夹具（M6-3）。
//!
//! 只用 **store**（method 0）方法、不压缩：`zip` crate 会校验 CRC，所以夹具必须给出正确的
//! CRC32 —— 手写一个约 10 行的位运算实现在夹具里就是全部代价；换成真压缩器只会让「夹具是不是
//! 被损坏了」这件事更难判断。

/// 用 store 方法打一个 zip（无需任何压缩依赖，上游的 `skill_import_archive` 只按条目读）。
pub(crate) fn zip_store(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, content) in entries {
        let name_bytes = name.as_bytes();
        let data = content.as_bytes();
        let crc = crc32(data);
        let offset = u32_of(out.len());

        write_u32(&mut out, 0x0403_4b50);
        write_u16(&mut out, 20);
        write_u16(&mut out, 0);
        write_u16(&mut out, 0);
        write_u16(&mut out, 0);
        write_u16(&mut out, 0);
        write_u32(&mut out, crc);
        write_u32(&mut out, u32_of(data.len()));
        write_u32(&mut out, u32_of(data.len()));
        write_u16(&mut out, u16_of(name_bytes.len()));
        write_u16(&mut out, 0);
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        write_u32(&mut directory, 0x0201_4b50);
        write_u16(&mut directory, 20);
        write_u16(&mut directory, 20);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u32(&mut directory, crc);
        write_u32(&mut directory, u32_of(data.len()));
        write_u32(&mut directory, u32_of(data.len()));
        write_u16(&mut directory, u16_of(name_bytes.len()));
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u32(&mut directory, 0);
        write_u32(&mut directory, offset);
        directory.extend_from_slice(name_bytes);
    }

    let directory_offset = u32_of(out.len());
    let directory_size = u32_of(directory.len());
    out.extend_from_slice(&directory);
    write_u32(&mut out, 0x0605_4b50);
    write_u16(&mut out, 0);
    write_u16(&mut out, 0);
    write_u16(&mut out, u16_of(entries.len()));
    write_u16(&mut out, u16_of(entries.len()));
    write_u32(&mut out, directory_size);
    write_u32(&mut out, directory_offset);
    write_u16(&mut out, 0);
    out
}

/// `usize` → ZIP 的 16 位字段（夹具的数据集永远够小，真越界就是夹具写错了）。
fn u16_of(value: usize) -> u16 {
    u16::try_from(value).expect("zip fixture field fits in u16")
}

/// `usize` → ZIP 的 32 位字段。
fn u32_of(value: usize) -> u32 {
    u32::try_from(value).expect("zip fixture field fits in u32")
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// CRC-32（IEEE 802.3），位运算版（无查表）。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// 夹具自身的自检（真跑一次解析，防止「CRC 写错 ⇒ 全部归档用例一起假红」）。
#[test]
fn zip_store_fixture_is_readable() {
    let archive = zip_store(&[("SKILL.md", "hello"), ("dir/other.md", "world")]);
    let parsed = mc_skill::archive::parse_skill_archive(&archive, "fixture.zip").expect("parse");
    assert_eq!(parsed.content, "hello");
    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].path, "dir/other.md");
    assert_eq!(parsed.files[0].content, "world");
}
