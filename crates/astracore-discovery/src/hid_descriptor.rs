//! Read-only HID report descriptor layout parser.

use serde::Serialize;
use std::{collections::BTreeMap, error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HidReportType {
    Input,
    Output,
    Feature,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HidReportSummary {
    pub report_type: HidReportType,
    pub report_id: Option<u8>,
    pub payload_bits: u64,
    pub payload_bytes: u64,
    /// Payload bytes plus the one-byte Report ID prefix, when present.
    pub transfer_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HidReportFieldInfo {
    pub report_type: HidReportType,
    pub report_id: Option<u8>,
    /// Offset within the payload; excludes the Report ID prefix.
    pub bit_offset: u64,
    pub bit_length: u64,
    pub report_size_bits: u32,
    pub report_count: u32,
    pub flags: u32,
    pub usage_page: Option<u32>,
    pub usages: Vec<u32>,
    pub usage_minimum: Option<u32>,
    pub usage_maximum: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HidReportDescriptorInfo {
    pub descriptor_size_bytes: usize,
    pub uses_report_ids: bool,
    pub long_items_skipped: usize,
    pub reports: Vec<HidReportSummary>,
    pub fields: Vec<HidReportFieldInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HidDescriptorErrorKind {
    TruncatedItem,
    ReservedItemType,
    InvalidItemSize,
    InvalidMainItem,
    InvalidReportId,
    MissingReportSize,
    MissingReportCount,
    MixedReportIds,
    GlobalStackUnderflow,
    UnclosedGlobalStack,
    CollectionUnderflow,
    UnclosedCollection,
    ArithmeticOverflow,
    NoReports,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HidDescriptorError {
    pub offset: usize,
    pub kind: HidDescriptorErrorKind,
}

impl fmt::Display for HidDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HID descriptor error at byte {}: {:?}",
            self.offset, self.kind
        )
    }
}

impl Error for HidDescriptorError {}

#[derive(Clone, Default)]
struct GlobalState {
    usage_page: Option<u32>,
    report_size: Option<u32>,
    report_count: Option<u32>,
    report_id: Option<u8>,
}

#[derive(Default)]
struct LocalState {
    usages: Vec<u32>,
    usage_minimum: Option<u32>,
    usage_maximum: Option<u32>,
}

fn error(offset: usize, kind: HidDescriptorErrorKind) -> HidDescriptorError {
    HidDescriptorError { offset, kind }
}

fn number(bytes: &[u8]) -> u32 {
    let mut value = 0_u32;
    for (index, byte) in bytes.iter().enumerate() {
        value |= u32::from(*byte) << (index * 8);
    }
    value
}

fn require_size(bytes: &[u8], size: usize, offset: usize) -> Result<(), HidDescriptorError> {
    if bytes.len() == size {
        Ok(())
    } else {
        Err(error(offset, HidDescriptorErrorKind::InvalidItemSize))
    }
}

fn require_value(bytes: &[u8], offset: usize) -> Result<u32, HidDescriptorError> {
    if bytes.is_empty() {
        Err(error(offset, HidDescriptorErrorKind::InvalidItemSize))
    } else {
        Ok(number(bytes))
    }
}

fn take_item_data<'a>(
    descriptor: &'a [u8],
    offset: &mut usize,
    size: usize,
    item_offset: usize,
) -> Result<&'a [u8], HidDescriptorError> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| error(item_offset, HidDescriptorErrorKind::ArithmeticOverflow))?;
    let data = descriptor
        .get(*offset..end)
        .ok_or_else(|| error(item_offset, HidDescriptorErrorKind::TruncatedItem))?;
    *offset = end;
    Ok(data)
}

/// Parse report layout only; no HID reports are sent or received.
pub fn parse_hid_report_descriptor(
    descriptor: &[u8],
) -> Result<HidReportDescriptorInfo, HidDescriptorError> {
    let mut offset = 0;
    let mut globals = GlobalState::default();
    let mut stack = Vec::new();
    let mut locals = LocalState::default();
    let mut collection_depth = 0_usize;
    let mut uses_report_ids = false;
    let mut long_items_skipped = 0;
    let mut report_bits: BTreeMap<(Option<u8>, HidReportType), u64> = BTreeMap::new();
    let mut fields = Vec::new();

    while let Some(&prefix) = descriptor.get(offset) {
        let item_offset = offset;
        offset += 1;

        if prefix == 0xfe {
            let header = take_item_data(descriptor, &mut offset, 2, item_offset)?;
            let size = header
                .first()
                .copied()
                .ok_or_else(|| error(item_offset, HidDescriptorErrorKind::TruncatedItem))?;
            take_item_data(descriptor, &mut offset, usize::from(size), item_offset)?;
            long_items_skipped += 1;
            continue;
        }

        let size = match prefix & 0x03 {
            3 => 4,
            value => usize::from(value),
        };
        let data = take_item_data(descriptor, &mut offset, size, item_offset)?;
        let item_type = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;

        match item_type {
            0 => {
                match tag {
                    8 | 9 | 11 => {
                        let report_type = match tag {
                            8 => HidReportType::Input,
                            9 => HidReportType::Output,
                            _ => HidReportType::Feature,
                        };
                        let flags = require_value(data, item_offset)?;
                        let report_size =
                            globals
                                .report_size
                                .filter(|size| *size > 0)
                                .ok_or_else(|| {
                                    error(item_offset, HidDescriptorErrorKind::MissingReportSize)
                                })?;
                        let report_count = globals
                            .report_count
                            .filter(|count| *count > 0)
                            .ok_or_else(|| {
                                error(item_offset, HidDescriptorErrorKind::MissingReportCount)
                            })?;
                        if uses_report_ids && globals.report_id.is_none() {
                            return Err(error(item_offset, HidDescriptorErrorKind::MixedReportIds));
                        }
                        let bit_length = u64::from(report_size)
                            .checked_mul(u64::from(report_count))
                            .ok_or_else(|| {
                                error(item_offset, HidDescriptorErrorKind::ArithmeticOverflow)
                            })?;
                        let entry = report_bits
                            .entry((globals.report_id, report_type))
                            .or_default();
                        let bit_offset = *entry;
                        *entry = entry.checked_add(bit_length).ok_or_else(|| {
                            error(item_offset, HidDescriptorErrorKind::ArithmeticOverflow)
                        })?;
                        fields.push(HidReportFieldInfo {
                            report_type,
                            report_id: globals.report_id,
                            bit_offset,
                            bit_length,
                            report_size_bits: report_size,
                            report_count,
                            flags,
                            usage_page: globals.usage_page,
                            usages: std::mem::take(&mut locals.usages),
                            usage_minimum: locals.usage_minimum,
                            usage_maximum: locals.usage_maximum,
                        });
                    }
                    10 => {
                        require_size(data, 1, item_offset)?;
                        collection_depth = collection_depth.checked_add(1).ok_or_else(|| {
                            error(item_offset, HidDescriptorErrorKind::ArithmeticOverflow)
                        })?;
                    }
                    12 => {
                        require_size(data, 0, item_offset)?;
                        collection_depth = collection_depth.checked_sub(1).ok_or_else(|| {
                            error(item_offset, HidDescriptorErrorKind::CollectionUnderflow)
                        })?;
                    }
                    _ => return Err(error(item_offset, HidDescriptorErrorKind::InvalidMainItem)),
                }
                locals = LocalState::default();
            }
            1 => match tag {
                0 => globals.usage_page = Some(require_value(data, item_offset)?),
                7 => globals.report_size = Some(require_value(data, item_offset)?),
                8 => {
                    require_size(data, 1, item_offset)?;
                    let report_id = data.first().copied().ok_or_else(|| {
                        error(item_offset, HidDescriptorErrorKind::InvalidItemSize)
                    })?;
                    if report_id == 0 {
                        return Err(error(item_offset, HidDescriptorErrorKind::InvalidReportId));
                    }
                    if report_bits.keys().any(|(id, _)| id.is_none()) {
                        return Err(error(item_offset, HidDescriptorErrorKind::MixedReportIds));
                    }
                    globals.report_id = Some(report_id);
                    uses_report_ids = true;
                }
                9 => globals.report_count = Some(require_value(data, item_offset)?),
                10 => {
                    require_size(data, 0, item_offset)?;
                    stack.push(globals.clone());
                }
                11 => {
                    require_size(data, 0, item_offset)?;
                    globals = stack.pop().ok_or_else(|| {
                        error(item_offset, HidDescriptorErrorKind::GlobalStackUnderflow)
                    })?;
                }
                _ => { /* Other Global items do not affect report layout. */ }
            },
            2 => match tag {
                0 => locals.usages.push(require_value(data, item_offset)?),
                1 => locals.usage_minimum = Some(require_value(data, item_offset)?),
                2 => locals.usage_maximum = Some(require_value(data, item_offset)?),
                _ => { /* Other Local items do not affect report layout. */ }
            },
            _ => return Err(error(item_offset, HidDescriptorErrorKind::ReservedItemType)),
        }
    }

    if collection_depth != 0 {
        return Err(error(offset, HidDescriptorErrorKind::UnclosedCollection));
    }
    if !stack.is_empty() {
        return Err(error(offset, HidDescriptorErrorKind::UnclosedGlobalStack));
    }
    if report_bits.is_empty() {
        return Err(error(offset, HidDescriptorErrorKind::NoReports));
    }

    let mut reports = Vec::with_capacity(report_bits.len());
    for ((report_id, report_type), payload_bits) in report_bits {
        let payload_bytes = payload_bits
            .checked_add(7)
            .ok_or_else(|| error(offset, HidDescriptorErrorKind::ArithmeticOverflow))?
            / 8;
        let transfer_bytes = payload_bytes
            .checked_add(u64::from(report_id.is_some()))
            .ok_or_else(|| error(offset, HidDescriptorErrorKind::ArithmeticOverflow))?;
        reports.push(HidReportSummary {
            report_type,
            report_id,
            payload_bits,
            payload_bytes,
            transfer_bytes,
        });
    }

    Ok(HidReportDescriptorInfo {
        descriptor_size_bytes: descriptor.len(),
        uses_report_ids,
        long_items_skipped,
        reports,
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::{HidDescriptorErrorKind, HidReportType, parse_hid_report_descriptor};

    fn summary(bytes: &[u8], report_id: Option<u8>, report_type: HidReportType) -> (u64, u64, u64) {
        let info = parse_hid_report_descriptor(bytes).unwrap();
        let report = info
            .reports
            .iter()
            .find(|report| report.report_id == report_id && report.report_type == report_type)
            .unwrap();
        (
            report.payload_bits,
            report.payload_bytes,
            report.transfer_bytes,
        )
    }

    #[test]
    fn parses_aura_descriptor() {
        let descriptor = vec![
            0x06, 0x0a, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x41, 0x09, 0x82, 0x75, 0x08, 0x95,
            0x3f, 0x15, 0x00, 0x26, 0xff, 0x00, 0x81, 0x02, 0x09, 0x02, 0x75, 0x08, 0x95, 0x3f,
            0x15, 0x00, 0x26, 0xff, 0x00, 0x91, 0x02, 0xc0,
        ];
        let info = parse_hid_report_descriptor(descriptor.as_slice()).unwrap();

        assert_eq!(info.descriptor_size_bytes, 36);
        assert!(info.uses_report_ids);
        assert_eq!(info.reports.len(), 2);
        assert_eq!(
            summary(descriptor.as_slice(), Some(0x41), HidReportType::Input),
            (504, 63, 64)
        );
        assert_eq!(
            summary(descriptor.as_slice(), Some(0x41), HidReportType::Output),
            (504, 63, 64)
        );
        assert!(
            !info
                .reports
                .iter()
                .any(|report| report.report_type == HidReportType::Feature)
        );
        assert_eq!(info.fields[0].usage_page, Some(0xff0a));
        assert_eq!(info.fields[0].usages, vec![0x82]);
        assert_eq!(info.fields[1].usages, vec![0x02]);
    }

    #[test]
    fn parses_report_without_id() {
        let descriptor = vec![0x75, 0x08, 0x95, 0x02, 0x81, 0x02];
        let info = parse_hid_report_descriptor(descriptor.as_slice()).unwrap();
        assert!(!info.uses_report_ids);
        assert_eq!(
            summary(descriptor.as_slice(), None, HidReportType::Input),
            (16, 2, 2)
        );
    }

    #[test]
    fn separates_multiple_report_ids_and_types() {
        let descriptor = vec![
            0x85, 0x01, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0x85, 0x02, 0x95, 0x03, 0x91, 0x02,
            0xb1, 0x02,
        ];
        assert_eq!(
            summary(descriptor.as_slice(), Some(1), HidReportType::Input),
            (16, 2, 3)
        );
        assert_eq!(
            summary(descriptor.as_slice(), Some(2), HidReportType::Output),
            (24, 3, 4)
        );
        assert_eq!(
            summary(descriptor.as_slice(), Some(2), HidReportType::Feature),
            (24, 3, 4)
        );
    }

    #[test]
    fn rounds_non_aligned_bits_up_to_bytes() {
        let descriptor = vec![0x75, 0x01, 0x95, 0x09, 0x81, 0x02];
        assert_eq!(
            summary(descriptor.as_slice(), None, HidReportType::Input),
            (9, 2, 2)
        );
    }

    #[test]
    fn restores_global_state_after_pop() {
        let descriptor = vec![
            0x75, 0x08, 0x95, 0x01, 0xa4, 0x75, 0x04, 0x95, 0x03, 0x81, 0x02, 0xb4, 0x81, 0x02,
        ];
        let info = parse_hid_report_descriptor(descriptor.as_slice()).unwrap();
        assert_eq!(info.fields[0].bit_length, 12);
        assert_eq!(info.fields[1].bit_offset, 12);
        assert_eq!(info.fields[1].bit_length, 8);
        assert_eq!(
            summary(descriptor.as_slice(), None, HidReportType::Input),
            (20, 3, 3)
        );
    }

    #[test]
    fn restores_report_id_after_pop() {
        let descriptor = vec![
            0x85, 0x01, 0x75, 0x08, 0x95, 0x01, 0xa4, 0x85, 0x02, 0x81, 0x02, 0xb4, 0x81, 0x02,
        ];
        assert_eq!(
            summary(descriptor.as_slice(), Some(1), HidReportType::Input),
            (8, 1, 2)
        );
        assert_eq!(
            summary(descriptor.as_slice(), Some(2), HidReportType::Input),
            (8, 1, 2)
        );
    }

    #[test]
    fn skips_complete_long_item() {
        let descriptor = vec![
            0xfe, 0x02, 0x99, 0xaa, 0xbb, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,
        ];
        let info = parse_hid_report_descriptor(descriptor.as_slice()).unwrap();
        assert_eq!(info.long_items_skipped, 1);
        assert_eq!(info.reports[0].payload_bytes, 1);
    }

    #[test]
    fn reports_truncated_and_invalid_descriptors_without_panicking() {
        let cases: Vec<(Vec<u8>, HidDescriptorErrorKind)> = vec![
            (vec![0x75], HidDescriptorErrorKind::TruncatedItem),
            (
                vec![0xfe, 0x02, 0x99, 0xaa],
                HidDescriptorErrorKind::TruncatedItem,
            ),
            (vec![0x85, 0x00], HidDescriptorErrorKind::InvalidReportId),
            (vec![0xb4], HidDescriptorErrorKind::GlobalStackUnderflow),
            (vec![0xc0], HidDescriptorErrorKind::CollectionUnderflow),
            (
                vec![0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0x85, 0x01],
                HidDescriptorErrorKind::MixedReportIds,
            ),
        ];
        for (descriptor, expected) in cases {
            assert_eq!(
                parse_hid_report_descriptor(descriptor.as_slice())
                    .unwrap_err()
                    .kind,
                expected
            );
        }
    }

    #[test]
    fn handles_every_single_byte_prefix_without_panicking() {
        for prefix in u8::MIN..=u8::MAX {
            let descriptor = vec![prefix];
            let _ = parse_hid_report_descriptor(descriptor.as_slice());
        }
    }
}
