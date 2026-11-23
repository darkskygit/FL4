//! The AArch64 kernel's boot-time decisions on the host: its memory lists
//! (fl4-kernel-aarch64 bootmem.rs), its ACPI parsing (hw.rs, over
//! host-allocated tables), the GIC layout arithmetic and the cores'
//! capability check (fl4-arch-aarch64 gic_layout.rs, ap_caps.rs). Only the window, the console
//! and the boot core's MPIDR are replaced.
extern crate self as fl4_core;
extern crate self as fl4_arch_aarch64;

pub mod boot {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Region {
        pub start: u64,
        pub end: u64,
    }
    impl Region {
        pub const EMPTY: Region = Region { start: 0, end: 0 };
    }
}

/// Physical memory: a host buffer (`MEMORY`: its address and length), so
/// tables hold small physical addresses (an RSDT's are 32 bits).
pub mod layout {
    use std::cell::Cell;
    thread_local! {
        pub static MEMORY: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    }
    pub fn pptr(p: u64) -> u64 {
        let (base, len) = MEMORY.get();
        assert!(p < len, "read outside physical memory: {p:#x}");
        base + p
    }
}

/// The window maps all of the buffer.
pub mod mmu {
    pub fn in_window(start: u64, len: u64) -> bool {
        start.checked_add(len).is_some_and(|end| end <= super::layout::MEMORY.get().1)
    }
}

pub mod serial {
    pub const KIND_NONE: u64 = 0;
    pub const KIND_PL011: u64 = 1;
    pub const KIND_16550: u64 = 2;
}

pub mod smp {
    pub const PSCI_NONE: u64 = 0;
    pub const PSCI_SMC: u64 = 1;
    pub const PSCI_HVC: u64 = 2;
}

pub mod cpu {
    pub fn mpidr() -> u64 {
        0
    }
}

#[path = "../../crates/fl4-arch-aarch64/src/gic_layout.rs"]
pub mod gic_layout;
#[path = "../../crates/fl4-arch-aarch64/src/el0_access.rs"]
mod el0_access;
#[path = "../../crates/fl4-arch-aarch64/src/ap_caps.rs"]
mod ap_caps;
#[path = "../../crates/fl4-arch-aarch64/src/vmid.rs"]
mod vmid;
#[allow(dead_code)]
#[path = "../../crates/fl4-kernel-aarch64/src/bootmem.rs"]
mod bootmem;
#[allow(dead_code)]
#[path = "../../crates/fl4-kernel-aarch64/src/hw.rs"]
mod hw;
#[path = "../../crates/fl4-efi/src/efi_mmap.rs"]
mod efi_mmap;
#[path = "../../crates/fl4-kernel-aarch64/src/extra_bi.rs"]
mod extra_bi;

// ---- GIC ---------------------------------------------------------------------

#[test]
fn the_distributor_never_touches_the_special_intids() {
    assert_eq!(gic_layout::dist_lines(0), 32);
    assert_eq!(gic_layout::dist_lines(2), 96);
    assert_eq!(gic_layout::dist_lines(30), 992);
    // ITLinesNumber 31: 1024 lines, of which 1020..1023 are special.
    assert_eq!(gic_layout::dist_lines(31), 1020);
    assert_eq!(gic_layout::dist_lines(0xffff_ffff), 1020);
    assert_eq!(gic_layout::bank_bits(960, 1020), u32::MAX);
    assert_eq!(gic_layout::bank_bits(992, 1020), 0x0fff_ffff);
    assert_eq!(gic_layout::bank_bits(32, 64), u32::MAX);
    assert_eq!(gic_layout::bank_bits(64, 64), 0);
    // The registers init_dist writes stay inside the defined ones:
    // IPRIORITYR/ITARGETSR up to n = 254, IROUTER up to n = 1019.
    let lines = gic_layout::dist_lines(31);
    assert!((32..lines).step_by(4).all(|i| i / 4 <= 254));
    assert!((32..lines).all(|i| i <= 1019));
}

#[test]
fn gicv2_spis_go_to_the_boot_cores_interface() {
    assert_eq!(gic_layout::v2_targets(0x0404_0404), 0x0404_0404);
    assert_eq!(gic_layout::v2_targets(0x0000_0080), 0x8080_8080);
    // A uniprocessor GIC reads zero.
    assert_eq!(gic_layout::v2_targets(0), 0x0101_0101);
}

/// GICv2 in its Secure (or only) view reads 1022 from GICC_IAR for a group 1
/// interrupt unless AckCtl is set: the first SPI then never acknowledged
/// and core 0 spun in the exception vector. GICv2 keeps every interrupt in
/// group 0 with AckCtl set; GICv3 keeps group 1.
#[test]
fn gicv2_interrupts_are_group_0_and_acknowledged() {
    assert_eq!(gic_layout::spi_groups(false, u32::MAX), 0);
    assert_eq!(gic_layout::spi_groups(false, 0x0fff_ffff), 0);
    assert_eq!(gic_layout::spi_groups(true, u32::MAX), u32::MAX);
    assert_eq!(gic_layout::spi_groups(true, 0x0fff_ffff), 0x0fff_ffff);
    // EnableGrp0, EnableGrp1, AckCtl; FIQEn (bit 3) clear.
    assert_eq!(gic_layout::V2_GICC_CTLR, 0b111);
    assert_eq!(gic_layout::V2_GICC_CTLR & (1 << 3), 0);
}

/// Audit round 5 F5: SGIs only reach cores a target list can name. GICv2
/// has eight CPU interfaces; a core reading no interface (or several, or
/// another core's) would never answer a remote call. GICv3 names Aff0 16
/// and more only with the range selector, which both the distributor and
/// the sending CPU interface must support.
#[test]
fn sgis_only_go_to_cores_a_target_list_can_name() {
    assert_eq!(gic_layout::v2_interface(0x0404_0404), 4);
    assert_eq!(gic_layout::v2_interface(0), 0);
    assert!(gic_layout::v2_interface_ok(1, []));
    assert!(gic_layout::v2_interface_ok(0x80, [1, 2, 4]));
    assert!(!gic_layout::v2_interface_ok(0, []), "no interface");
    assert!(!gic_layout::v2_interface_ok(3, []), "two interfaces");
    assert!(!gic_layout::v2_interface_ok(0x100, []), "a ninth interface");
    assert!(!gic_layout::v2_interface_ok(2, [1, 2]), "another core's interface");
    assert_eq!(gic_layout::V2_INTERFACES, 8);
    let typer_rss = 1 << 26;
    let ctlr_rss = 1 << 18;
    assert!(gic_layout::v3_rss(typer_rss, ctlr_rss));
    assert!(!gic_layout::v3_rss(typer_rss, 0));
    assert!(!gic_layout::v3_rss(0, ctlr_rss));
    assert!(!gic_layout::v3_rss(!typer_rss, !ctlr_rss));
    assert!(gic_layout::v3_sgi_reaches(0, false));
    assert!(gic_layout::v3_sgi_reaches(15, false));
    assert!(!gic_layout::v3_sgi_reaches(16, false), "would reach Aff0 0");
    assert!(!gic_layout::v3_sgi_reaches(0xff, false));
    assert!(gic_layout::v3_sgi_reaches(16, true));
    assert!(gic_layout::v3_sgi_reaches(0xff, true));
}

/// GICR_TYPER values at frame addresses.
fn frames(list: &[(u64, u64)]) -> impl Fn(u64) -> u64 + '_ {
    move |a| list.iter().find(|f| f.0 == a).map(|f| f.1).unwrap_or_else(|| panic!("read of an unknown frame {a:#x}"))
}

const VLPIS: u64 = 1 << 1;
const LAST: u64 = 1 << 4;

#[test]
fn redistributors_are_found_only_inside_their_mapping() {
    let b = 0x800_0000;
    let t = [(b, 0 << 32), (b + 0x2_0000, 1 << 32), (b + 0x4_0000, 2 << 32 | LAST)];
    assert_eq!(gic_layout::find_frame(b, 0x6_0000, 1, frames(&t)), Some(b + 0x2_0000));
    assert_eq!(gic_layout::find_frame(b, 0x6_0000, 2, frames(&t)), Some(b + 0x4_0000));
    assert_eq!(gic_layout::find_frame(b, 0x6_0000, 3, frames(&t)), None);
    // A mapping that ends inside the third frame: never that one, and its
    // GICR_TYPER is not read (`frames` would panic).
    assert_eq!(gic_layout::find_frame(b, 0x5_0000, 2, frames(&t)), None);
    assert_eq!(gic_layout::find_frame(b, 0x1_0000, 0, frames(&t)), None);
    assert_eq!(gic_layout::find_frame(b, 0, 0, frames(&t)), None);
    // GICv4: 256 KiB per redistributor.
    let v4 = [(b, 0 << 32 | VLPIS), (b + 0x4_0000, 1 << 32 | VLPIS | LAST)];
    assert_eq!(gic_layout::find_frame(b, 0x8_0000, 1, frames(&v4)), Some(b + 0x4_0000));
    assert_eq!(gic_layout::find_frame(b, 0x6_0000, 1, frames(&v4)), Some(b + 0x4_0000));
    assert_eq!(gic_layout::find_frame(u64::MAX - 0xffff, 0x1_0000, 0, frames(&[])), None);
}

/// A device tree's `redistributor-stride` spaces redistributors further
/// apart than their frames: the walk steps by it, finds a later one, and
/// keeps the padding after the last one up to the range's end. A stride
/// smaller than a redistributor's frames is no layout at all.
#[test]
fn redistributors_follow_their_stride() {
    let b = 0x800_0000;
    let t = [(b, 0 << 32), (b + 0x3_0000, 1 << 32), (b + 0x6_0000, 2 << 32 | LAST)];
    assert_eq!(gic_layout::find_frame_strided(b, 0x9_0000, 0x3_0000, 1, frames(&t)), Some(b + 0x3_0000));
    assert_eq!(gic_layout::find_frame_strided(b, 0x9_0000, 0x3_0000, 2, frames(&t)), Some(b + 0x6_0000));
    assert_eq!(gic_layout::reserve_strided(b, 0x9_0000, 0x9_0000, 0x3_0000, frames(&t)), Some(0x9_0000));
    assert_eq!(gic_layout::reserve_strided(b, 0, 0x9_0000, 0x3_0000, frames(&t)), Some(0x9_0000));
    let mut seen = vec![];
    assert!(gic_layout::for_each_frame(b, 0x9_0000, 0x3_0000, frames(&t), |a, _| seen.push(a)));
    assert_eq!(seen, [b, b + 0x3_0000, b + 0x6_0000]);
    // GICv4 redistributors (256 KiB) with a 128 KiB stride.
    let v4 = [(b, VLPIS), (b + 0x4_0000, VLPIS | LAST)];
    assert_eq!(gic_layout::reserve_strided(b, 0x8_0000, 0x8_0000, 0x2_0000, frames(&v4)), None);
    assert_eq!(gic_layout::find_frame_strided(b, 0x8_0000, 0x2_0000, 0, frames(&v4)), None);
    assert!(!gic_layout::for_each_frame(b, 0x8_0000, 0x2_0000, frames(&v4), |_, _| {}));
}

/// A GICv3 device tree node with `extra` properties after its reg.
fn dt_gic_props(extra: &[(&str, Vec<u8>)]) -> Option<hw::Hw> {
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("intc@8000000");
    d.prop_str("compatible", "arm,gic-v3");
    d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0x40_0000)]));
    for (n, v) in extra {
        d.prop(n, v);
    }
    d.end();
    d.begin("pl011@9000000");
    d.prop_str("compatible", "arm,pl011");
    d.prop("reg", &reg2(&[(0x900_0000, 0x1000)]));
    d.end();
    d.end();
    discover_blob(&d.blob())
}

/// `redistributor-stride`: one 64-bit value, at least 128 KiB, a multiple
/// of 64 KiB; anything else leaves the redistributors unknown (the boot
/// fails). The distributor's and the console's sizes are kept.
#[test]
fn the_redistributor_stride_and_device_sizes_come_from_the_device_tree() {
    let h = dt_gic_props(&[]).expect("device tree");
    assert_eq!((h.gicr_stride, h.gicd_len, h.uart_base, h.uart_len), (0, 0x1_0000, 0x900_0000, 0x1000));
    let h = dt_gic_props(&[("redistributor-stride", 0x3_0000u64.to_be_bytes().to_vec())]).expect("device tree");
    assert_eq!(h.gicr_stride, 0x3_0000);
    assert!(!h.gicr_lost);
    for (what, v) in [
        ("32 bits", 0x4_0000u32.to_be_bytes().to_vec()),
        ("0", 0u64.to_be_bytes().to_vec()),
        ("64 KiB", 0x1_0000u64.to_be_bytes().to_vec()),
        ("misaligned", 0x3_1000u64.to_be_bytes().to_vec()),
    ] {
        let h = dt_gic_props(&[("redistributor-stride", v)]).expect("device tree");
        assert!(h.gicr_lost, "{what}");
    }
}

/// The redistributor walk refuses what it cannot follow: a range with no
/// whole redistributor in it, a base off a page or past the address space,
/// a stride off 64 KiB, a step past the address space; a last frame is
/// still visited when the step after it would overflow.
#[test]
fn a_redistributor_walk_refuses_what_it_cannot_follow() {
    let b = 0x800_0000;
    let one = [(b, LAST)];
    // Less than a redistributor's RD and SGI frames.
    assert!(!gic_layout::for_each_frame(b, 0x1_0000, 0, frames(&one), |_, _| {}));
    assert_eq!(gic_layout::reserve_strided(b, 0x1_0000, 0x1_0000, 0, frames(&one)), None);
    // A base off a page; a stride off 64 KiB; a range past the end.
    assert!(!gic_layout::for_each_frame(b + 0x800, 0x2_0000, 0, frames(&[]), |_, _| {}));
    assert!(!gic_layout::for_each_frame(b, 0x6_0000, 0x2_8000, frames(&one), |_, _| {}));
    assert!(!gic_layout::for_each_frame(u64::MAX - 0xffff, 0x2_0000, 0, frames(&[]), |_, _| {}));
    // The last frame at the top of the address space with a stride that
    // would step past it: visited, kept to the range's end.
    let top = u64::MAX - 0x3_ffff;
    let last = [(top, LAST)];
    let mut seen = vec![];
    assert!(gic_layout::for_each_frame(top, 0x3_ffff, 0x4_0000, frames(&last), |a, _| seen.push(a)));
    assert_eq!(seen, [top]);
    assert_eq!(gic_layout::reserve_strided(top, 0x3_ffff, 0x3_ffff, 0x4_0000, frames(&last)), Some(0x3_ffff));
    // A bounded range of 192 KiB with one packed redistributor that is not
    // the last: a second one begins inside it but does not fit, unseen.
    let first = [(b, 0)];
    assert!(!gic_layout::for_each_frame(b, 0x3_0000, 0, frames(&first), |_, _| {}));
    assert_eq!(gic_layout::reserve_strided(b, 0x3_0000, 0x3_0000, 0, frames(&first)), None);
    // Two that fill it exactly, neither marked last: whole.
    let two = [(b, 0), (b + 0x2_0000, 1 << 32)];
    let mut seen = vec![];
    assert!(gic_layout::for_each_frame(b, 0x4_0000, 0, frames(&two), |a, _| seen.push(a)));
    assert_eq!(seen, [b, b + 0x2_0000]);
    assert_eq!(gic_layout::reserve_strided(b, 0x4_0000, 0x4_0000, 0, frames(&two)), Some(0x4_0000));
    // A MADT GICC range (RD and SGI frames) of a GICv4 redistributor: its
    // VLPI frames run past the range, which is whole.
    assert_eq!(gic_layout::reserve_strided(b, 0x2_0000, 0x2_0000, 0, frames(&[(b, VLPIS)])), Some(0x4_0000));
    // A frame that is not the last, whose step would overflow.
    let not_last = [(top, 0)];
    assert!(!gic_layout::for_each_frame(top, 0x3_ffff, 0x4_0000, frames(&not_last), |_, _| {}));
    assert_eq!(gic_layout::reserve_strided(top, 0, 0x3_ffff, 0x4_0000, frames(&not_last)), None);
}

#[test]
fn a_gicv4_redistributor_reserves_its_vlpi_frames() {
    let b = 0x800_0000;
    // A MADT GICC entry's range: one core's RD and SGI frames.
    assert_eq!(gic_layout::span(b, 0x2_0000, frames(&[(b, VLPIS)])), 0x4_0000);
    assert_eq!(gic_layout::span(b, 0x2_0000, frames(&[(b, 0)])), 0x2_0000);
    // A GICR range: all of it, the last frame's VLPI frames included.
    let v4 = [(b, VLPIS), (b + 0x4_0000, VLPIS | LAST)];
    assert_eq!(gic_layout::span(b, 0x8_0000, frames(&v4)), 0x8_0000);
    assert_eq!(gic_layout::span(b, 0x1_0000, frames(&v4)), 0);
}

// ---- cache maintenance ------------------------------------------------------------

/// Audit F1: on GICv2 another core's SGIs and PPIs (IDs 0..31) are banked
/// in its own CPU interface's view of the distributor, so masking,
/// unmasking or configuring one from here takes a remote call (an IRQ
/// handler deleted on core 0 for core 1's PPI must mask it on core 1).
/// This core's, every GICv3 one (the redistributors) and SPIs are direct.
#[test]
fn another_cores_gicv2_banked_interrupts_take_a_remote_call() {
    for id in [0, 15, 16, 27, 31] {
        assert!(gic_layout::banked_remote(false, id, 1, 0), "id {id}");
        assert!(gic_layout::banked_remote(false, id, 0, 3), "id {id}");
        assert!(!gic_layout::banked_remote(false, id, 2, 2), "id {id}");
        assert!(!gic_layout::banked_remote(true, id, 1, 0), "id {id}");
    }
    for id in [32, 33, 1019] {
        assert!(!gic_layout::banked_remote(false, id, 1, 0), "id {id}");
        assert!(!gic_layout::banked_remote(true, id, 1, 0), "id {id}");
    }
}

/// The interrupt ID of an acknowledge value: GICv2 keeps the low ten bits
/// (an SGI's source CPU is above them); GICv3's INTIDs are whole, so an
/// extended SPI or PPI (1024..8191) is not taken for the SGI or SPI with
/// its low ten bits. Only 1020..=1023 are special (nothing to end); a real
/// interrupt the kernel has no IRQ for (1024 up to the first LPI) is
/// unsupported: ended, then dropped.
#[test]
fn acknowledge_values_name_their_interrupt() {
    const LPI: u64 = 8192;
    // GICv2: SGI 1 from CPU 3, SPI 40, 1022 and 1023.
    assert_eq!(gic_layout::raw_id(false, 3 << 10 | 1), 1);
    assert!(!gic_layout::is_spurious(false, 3 << 10 | 1));
    assert!(!gic_layout::is_unsupported(false, 3 << 10 | 1, LPI));
    assert_eq!(gic_layout::raw_id(false, 40), 40);
    assert!(!gic_layout::is_spurious(false, 40));
    for id in [1020, 1022, 1023] {
        assert!(gic_layout::is_spurious(false, id), "{id}");
        assert!(!gic_layout::is_unsupported(false, id, LPI), "{id}");
    }
    for id in [0, 1, 27, 32, 1019] {
        assert_eq!(gic_layout::raw_id(true, id), id);
        assert!(!gic_layout::is_spurious(true, id), "{id}");
        assert!(!gic_layout::is_unsupported(true, id, LPI), "{id}");
    }
    for id in 1020..=1023 {
        assert!(gic_layout::is_spurious(true, id), "{id}");
        assert!(!gic_layout::is_unsupported(true, id, LPI), "{id}");
    }
    // Extended SPIs and PPIs: real interrupts, not SGI 0 (4096 & 0x3ff) or
    // SPI 32, and not special either.
    for id in [1024, 1056, 4096, 4096 + 32, 5119, 8191] {
        assert_eq!(gic_layout::raw_id(true, id), id);
        assert!(!gic_layout::is_spurious(true, id), "{id}");
        assert!(gic_layout::is_unsupported(true, id, LPI), "{id}");
    }
    // LPIs, the kernel's and past them: neither.
    for id in [LPI, LPI + 255, LPI + 256, 16383, 1 << 20] {
        assert_eq!(gic_layout::raw_id(true, id), id);
        assert!(!gic_layout::is_spurious(true, id), "{id}");
        assert!(!gic_layout::is_unsupported(true, id, LPI), "{id}");
    }
}

/// GICD_TYPER: LPIs need LPIS, 14 interrupt ID bits, and, where num_LPIs
/// bounds them, room for the kernel's 256.
#[test]
fn the_distributor_has_every_lpi_the_kernel_numbers() {
    let typer = |id_bits: u32, num_lpis: u32| 1 << 17 | (id_bits - 1) << 19 | num_lpis << 11;
    assert!(gic_layout::lpi_supported(true, typer(16, 0), 14, 256));
    assert!(gic_layout::lpi_supported(true, typer(14, 0), 14, 256));
    assert!(!gic_layout::lpi_supported(true, typer(13, 0), 14, 256), "too few ID bits");
    assert!(!gic_layout::lpi_supported(true, typer(16, 5), 14, 256), "64 LPIs");
    assert!(!gic_layout::lpi_supported(true, typer(16, 6), 14, 256), "128 LPIs");
    assert!(gic_layout::lpi_supported(true, typer(16, 7), 14, 256), "256 LPIs");
    assert!(gic_layout::lpi_supported(true, typer(16, 31), 14, 256));
    assert!(!gic_layout::lpi_supported(true, typer(16, 0) & !(1 << 17), 14, 256), "no LPIS");
    assert!(!gic_layout::lpi_supported(false, typer(16, 0), 14, 256), "GICv2");
}

/// The ITS and redistributor table base registers are used only as they
/// read back: valid, flat, the size and page size asked for, at the
/// address asked for (64 KiB pages keep PA[51:48] in bits 12..15).
#[test]
fn table_base_registers_read_back_as_asked() {
    use gic_layout::{BASER_PSZ_16K as P16, BASER_PSZ_4K as P4, BASER_PSZ_64K as P64, BASER_VALID as V};
    let pa = 0x4_0002_0000u64;
    let b = |psz: u64, pages: u64, pa: u64| V | 7 << 59 | 1 << 56 | pa | 1 << 10 | psz << 8 | (pages - 1);
    assert!(gic_layout::its_baser_ok(b(P64, 1, pa), pa, P64, 1));
    assert!(gic_layout::its_baser_ok(b(P16, 4, pa), pa, P16, 4));
    assert!(gic_layout::its_baser_ok(b(P4, 16, pa), pa, P4, 16));
    assert!(!gic_layout::its_baser_ok(b(P64, 1, pa) & !V, pa, P64, 1), "not valid");
    assert!(!gic_layout::its_baser_ok(b(P64, 1, pa) | 1 << 62, pa, P64, 1), "indirect");
    assert!(!gic_layout::its_baser_ok(b(P16, 1, pa), pa, P64, 1), "page size");
    assert!(!gic_layout::its_baser_ok(b(P64, 2, pa), pa, P64, 1), "size");
    assert!(!gic_layout::its_baser_ok(b(P64, 1, 0x1_0000), pa, P64, 1), "address");
    // A 52-bit address in 64 KiB pages: PA[51:48] in bits 12..15.
    let hi = 0x000a_0000_0001_0000u64;
    assert_eq!(gic_layout::its_baser_pa(V | 0x1_0000 | 0xa << 12 | P64 << 8, P64), hi);
    assert!(!gic_layout::its_baser_ok(b(P64, 1, 0x1_0000) | 0xa << 12, 0x1_0000, P64, 1), "PA[51:48] set");

    let cmd = 0x8_0001_0000u64;
    assert!(gic_layout::its_cbaser_ok(V | 7 << 59 | cmd | 1 << 10 | 15, cmd, 16));
    assert!(!gic_layout::its_cbaser_ok(cmd | 15, cmd, 16), "not valid");
    assert!(!gic_layout::its_cbaser_ok(V | cmd | 3, cmd, 16), "size");
    assert!(!gic_layout::its_cbaser_ok(V | (cmd + 0x1000) | 15, cmd, 16), "address");

    let prop = 0x8_0000_0000u64;
    assert!(gic_layout::propbaser_ok(prop | 7 << 7 | 1 << 10 | 13, prop, 14));
    assert!(!gic_layout::propbaser_ok(prop | 7 << 7 | 1 << 10 | 15, prop, 14), "ID bits");
    assert!(!gic_layout::propbaser_ok(0x1000 | 13, prop, 14), "address");
    let pend = 0x8_0006_0000u64;
    assert!(gic_layout::pendbaser_ok(pend | 7 << 7 | 1 << 10, pend));
    assert!(!gic_layout::pendbaser_ok(pend + 0x1_0000, pend));
    // Attributes compared across redistributors: shareability and both
    // cacheabilities, nothing else.
    assert_eq!(gic_layout::gicr_baser_attrs(prop | 7 << 7 | 1 << 10 | 13), 7 << 7 | 1 << 10);
    assert_ne!(gic_layout::gicr_baser_attrs(1 << 7), gic_layout::gicr_baser_attrs(7 << 7 | 1 << 10));
}

/// An ITS is kept from user space whole. A size the tables give is the
/// ITS's (a GICv4.0 ITS has two frames, and the next device may follow),
/// unless it is less than two frames, or than three for an ITS showing
/// GICv4.1 features (VSGI, VMAPP, SVPET, nID): the tables contradict the
/// hardware. With none given (a MADT): three frames for any GICv4 ITS
/// (GITS_PIDR2.ArchRev 4) or one showing GICv4.1 features, two otherwise.
/// VMOVP (bit 37, GICv4.0) adds nothing.
#[test]
fn an_its_is_kept_whole() {
    assert_eq!(gic_layout::ITS_SIZE, 0x2_0000);
    assert_eq!(gic_layout::ITS_SIZE_V4_1, 0x3_0000);
    assert_eq!(gic_layout::GITS_PIDR2, 0xffe8);
    // GICv3, as QEMU's (Physical, ITT entry size 8, 16 ID bits, PTA).
    let v3 = 1 | 7 << 4 | 15 << 8 | 15 << 13 | 1 << 19;
    let (rev3, rev4) = (0x3b, 0x4b);
    // Unsized (MADT).
    assert_eq!(gic_layout::its_span(v3, rev3, 0), Some(0x2_0000));
    assert_eq!(gic_layout::its_span(v3 | 1 << 37, rev3, 0), Some(0x2_0000), "VMOVP");
    assert_eq!(gic_layout::its_span(v3, rev4, 0), Some(0x3_0000), "GICv4, VSGI clear");
    assert_eq!(gic_layout::its_span(v3 | 1 << 39, rev4, 0), Some(0x3_0000));
    for bit in [39, 40, 41, 42, 43] {
        assert_eq!(gic_layout::its_span(v3 | 1 << bit, rev3, 0), Some(0x3_0000), "bit {bit}");
    }
    // Sized: honoured, a GICv4.0 ITS's two frames included.
    assert_eq!(gic_layout::its_span(v3, rev3, 0x2_0000), Some(0x2_0000));
    assert_eq!(gic_layout::its_span(v3 | 1 << 37, rev4, 0x2_0000), Some(0x2_0000), "sized GICv4.0");
    assert_eq!(gic_layout::its_span(v3 | 1 << 39, rev4, 0x3_0000), Some(0x3_0000), "sized GICv4.1 (VSGI)");
    assert_eq!(gic_layout::its_span(v3 | 1 << 40, rev4, 0x3_0000), Some(0x3_0000), "sized GICv4.1 (VMAPP)");
    assert_eq!(gic_layout::its_span(v3 | 1 << 41, rev4, 0x3_0000), Some(0x3_0000), "sized GICv4.1 (SVPET)");
    assert_eq!(gic_layout::its_span(v3, rev3, 0x4_0000), Some(0x4_0000));
    assert_eq!(gic_layout::its_span(v3 | 1 << 39, rev4, 0x10_0000), Some(0x10_0000));
    // Sized against the hardware: under two frames, or a GICv4.1 ITS in two.
    assert_eq!(gic_layout::its_span(v3, rev3, 0x1_0000), None);
    assert_eq!(gic_layout::its_span(v3 | 1 << 39, rev4, 0x2_0000), None, "GICv4.1 in two frames");
    assert_eq!(gic_layout::its_span(v3 | 1 << 43, rev4, 0x2_0000), None, "nID in two frames");
}

/// Extended SPIs (GICD_TYPER.ESPI, ESPI_range) and extended PPIs
/// (GICR_TYPER.PPInum) the kernel disables.
#[test]
fn extended_interrupts_are_counted() {
    assert_eq!(gic_layout::espi_count(0), 0);
    assert_eq!(gic_layout::espi_count(31 << 27), 0, "ESPI_range without ESPI");
    assert_eq!(gic_layout::espi_count(1 << 8), 32);
    assert_eq!(gic_layout::espi_count(1 << 8 | 3 << 27), 128);
    assert_eq!(gic_layout::espi_count(1 << 8 | 31 << 27), 1024);
    assert_eq!(gic_layout::eppi_words(0), 0);
    assert_eq!(gic_layout::eppi_words(1 << 27), 1);
    assert_eq!(gic_layout::eppi_words(2 << 27), 2);
    assert_eq!(gic_layout::eppi_words(31 << 27), 2, "reserved values: no more than 64");
    assert_eq!(gic_layout::eppi_words(0xffff_ffff_0000_0000 | 1 << 4), 0, "other fields");
}

/// A MADT GIC ITS entry (type 0x0f, `len` bytes, 20 in ACPI 6.6).
fn its_entry_len(id: u32, base: u64, len: u8) -> Vec<u8> {
    let mut e = vec![0u8; 20];
    e[0] = 0x0f;
    e[1] = len;
    e[4..8].copy_from_slice(&id.to_le_bytes());
    e[8..16].copy_from_slice(&base.to_le_bytes());
    e.truncate(len as usize);
    e
}

fn its_entry(id: u32, base: u64) -> Vec<u8> {
    its_entry_len(id, base, 20)
}

fn madt_hw(e: &[Vec<u8>]) -> hw::Hw {
    let mut all = vec![gicd(), gicc(0, 0x80a_0000)];
    all.extend_from_slice(e);
    let t = tables(&madt(&all), 2, b"XSDT");
    hw::discover(t.rsdp(), 0).expect("ACPI")
}

/// Every ITS the MADT gives is recorded (once each), not the first only;
/// more than the kernel records, or one it cannot place (a short entry, at
/// 0 or off 64 KiB), fails the boot.
#[test]
fn every_its_of_the_madt_is_recorded() {
    let h = madt_hw(&[its_entry(0, 0x808_0000), its_entry(1, 0x10_0000_0000), its_entry(2, 0x808_0000)]);
    assert_eq!(&h.its[..h.n_its], &[its_block(0x808_0000, 0, u64::MAX), its_block(0x10_0000_0000, 0, u64::MAX)]);
    assert!(!h.its_lost);

    for (what, e) in [
        ("at 0", its_entry(3, 0)),
        ("off 64 KiB", its_entry(3, 0x808_1000)),
    ] {
        let h = madt_hw(&[its_entry(0, 0x808_0000), e]);
        assert!(h.its_lost, "{what}");
    }
    // A short ITS entry makes the MADT unusable.
    let t = tables(&madt(&[gicd(), gicc(0, 0x80a_0000), its_entry_len(3, 0x900_0000, 16)]), 2, b"XSDT");
    assert!(hw::discover(t.rsdp(), 0).is_none(), "16-byte ITS entry");

    let mut e = vec![];
    for i in 0..hw::MAX_ITS as u64 {
        e.push(its_entry(i as u32, 0x1000_0000 + i * 0x10_0000));
    }
    let h = madt_hw(&e);
    assert_eq!(h.n_its, hw::MAX_ITS);
    assert!(!h.its_lost);
    e.push(its_entry(99, 0x2000_0000));
    let h = madt_hw(&e);
    assert_eq!(h.n_its, hw::MAX_ITS);
    assert!(h.its_lost);
}

fn its_block(base: u64, raw_len: u64, max_len: u64) -> hw::ItsBlock {
    hw::ItsBlock { base, raw_len, max_len }
}

/// A MADT whose entries are cut short, run past it, or stop short of its
/// end is not used: no hardware is published from it.
#[test]
fn a_malformed_madt_is_not_used() {
    let good = madt(&[gicd(), gicc(0, 0x80a_0000), its_entry(0, 0x808_0000)]);
    let t = tables(&good, 2, b"XSDT");
    assert!(hw::discover(t.rsdp(), 0).is_some());
    // An entry of length 1.
    let mut short = good.clone();
    short.extend_from_slice(&[0x7f, 1]);
    // An entry running past the table.
    let mut past = good.clone();
    past.extend_from_slice(&[0x7f, 8, 0, 0]);
    // One trailing byte.
    let mut trailing = good.clone();
    trailing.push(0);
    // GIC entries too short for what they hold.
    let undersized = |ty: u8, len: u8| {
        let mut e = vec![0u8; len as usize];
        e[0] = ty;
        e[1] = len;
        madt(&[gicd(), gicc(0, 0x80a_0000), e])
    };
    for (what, e) in [
        ("length 1", short),
        ("past the end", past),
        ("trailing byte", trailing),
        ("12-byte GICR", undersized(0x0e, 12)),
        ("40-byte GICC", undersized(0x0b, 40)),
        ("16-byte GICD", undersized(0x0c, 16)),
    ] {
        let t = tables(&e, 2, b"XSDT");
        assert!(hw::discover(t.rsdp(), 0).is_none(), "{what}");
    }
    // Bad checksums: the XSDT's, the MADT's.
    for at in [BASE as usize + 64 + 9, BASE as usize + 128 + 9] {
        let mut t = tables(&good, 2, b"XSDT");
        t.mem[at] = t.mem[at].wrapping_add(1);
        assert!(hw::discover(t.rsdp(), 0).is_none(), "checksum at {at:#x}");
    }
}

/// A device tree with a GICv3 node (with `gic_ranges` as its `ranges`
/// property, if any) under a bus node (`bus_ranges`, if any, of
/// (child, parent, size) entries, two cells each) and ITS nodes under it
/// with `its_regs` (each a reg property's raw bytes).
fn dt_its_bus(bus_ranges: Option<&[(u64, u64, u64)]>, gic_ranges: Option<&[u8]>, its_regs: &[Vec<u8>]) -> hw::Hw {
    dt_its_bus_try(bus_ranges, gic_ranges, its_regs).expect("device tree")
}

fn dt_its_bus_try(bus_ranges: Option<&[(u64, u64, u64)]>, gic_ranges: Option<&[u8]>, its_regs: &[Vec<u8>]) -> Option<hw::Hw> {
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("soc");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    if let Some(r) = bus_ranges {
        let raw: Vec<u8> = r.iter().flat_map(|&(c, p, s)| [c.to_be_bytes(), p.to_be_bytes(), s.to_be_bytes()].concat()).collect();
        d.prop("ranges", &raw);
    }
    d.begin("intc@8000000");
    d.prop_str("compatible", "arm,gic-v3");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    if let Some(r) = gic_ranges {
        d.prop("ranges", r);
    }
    d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0x2_0000)]));
    for (i, r) in its_regs.iter().enumerate() {
        d.begin(&format!("msi-controller@{i}"));
        d.prop_str("compatible", "arm,gic-v3-its");
        d.prop("reg", r);
        d.end();
    }
    d.end();
    d.end();
    d.end();
    let blob = d.blob();
    let mut mem = vec![0u8; BASE as usize + blob.len()];
    mem[BASE as usize..].copy_from_slice(&blob);
    layout::MEMORY.set((mem.as_ptr() as u64, mem.len() as u64));
    let h = hw::discover(0, BASE);
    layout::MEMORY.set((0, 0));
    h
}

fn discover_blob(blob: &[u8]) -> Option<hw::Hw> {
    let mut mem = vec![0u8; BASE as usize + blob.len()];
    mem[BASE as usize..].copy_from_slice(blob);
    layout::MEMORY.set((mem.as_ptr() as u64, mem.len() as u64));
    let h = hw::discover(0, BASE);
    layout::MEMORY.set((0, 0));
    h
}

/// A GICv3 device tree (`dt_hw`'s) with `extra` nodes nested `depth` deep
/// at its end.
fn gic_dtb(depth: usize) -> Vec<u8> {
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("intc@8000000");
    d.prop_str("compatible", "arm,gic-v3");
    d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0x2_0000)]));
    d.end();
    for i in 0..depth {
        d.begin(&format!("n{i}"));
        d.prop_u32("x", 0);
    }
    for _ in 0..depth {
        d.end();
    }
    d.end();
    d.blob()
}

/// A blob that is not whole and well formed is not used at all: no
/// FDT_END, a node left open, nodes deeper than the kernel follows.
#[test]
fn a_malformed_device_tree_is_not_used() {
    assert!(discover_blob(&gic_dtb(0)).is_some());
    // Root at depth 1: 14 more levels make 16, the most followed.
    assert!(discover_blob(&gic_dtb(15)).is_some());
    assert!(discover_blob(&gic_dtb(16)).is_none(), "17 levels");
    let st = |b: &[u8], at: usize| u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
    // FDT_END turned into FDT_NOP: the walk runs off the block.
    let mut b = gic_dtb(0);
    let end_tok = st(&b, 8) + st(&b, 36) - 4;
    assert_eq!(st(&b, end_tok), 9);
    b[end_tok..end_tok + 4].copy_from_slice(&4u32.to_be_bytes());
    assert!(discover_blob(&b).is_none(), "no FDT_END");
    // The root's FDT_END_NODE turned into FDT_NOP: a node left open.
    let mut b = gic_dtb(0);
    assert_eq!(st(&b, end_tok - 4), 2);
    b[end_tok - 4..end_tok].copy_from_slice(&4u32.to_be_bytes());
    assert!(discover_blob(&b).is_none(), "unclosed node");

    // An FDT_END before the end of the block, tokens after it.
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("intc@8000000");
    d.prop_str("compatible", "arm,gic-v3");
    d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0x2_0000)]));
    d.end();
    d.end();
    d.word(9);
    d.word(4);
    assert!(discover_blob(&d.blob()).is_none(), "early FDT_END");
    // A property after a child node.
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("intc@8000000");
    d.prop_str("compatible", "arm,gic-v3");
    d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0x2_0000)]));
    d.end();
    d.prop_u32("model", 0);
    d.end();
    assert!(discover_blob(&d.blob()).is_none(), "property after a child");
    // Two root nodes; a named root. Each holds a GIC the kernel would
    // otherwise find.
    let gic_root = |d: &mut Dtb, name: &str| {
        d.begin(name);
        d.prop_u32("#address-cells", 2);
        d.prop_u32("#size-cells", 2);
        d.begin("intc@8000000");
        d.prop_str("compatible", "arm,gic-v3");
        d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0x2_0000)]));
        d.end();
        d.end();
    };
    let mut d = Dtb::default();
    gic_root(&mut d, "");
    assert!(discover_blob(&d.blob()).is_some(), "one root");
    let mut d = Dtb::default();
    gic_root(&mut d, "");
    d.begin("");
    d.end();
    assert!(discover_blob(&d.blob()).is_none(), "two roots");
    let mut d = Dtb::default();
    gic_root(&mut d, "root");
    assert!(discover_blob(&d.blob()).is_none(), "named root");
    // The strings block ends inside a property's name: a trailing one,
    // after everything the kernel uses.
    let trailing = || {
        let mut d = Dtb::default();
        d.begin("");
        d.prop_u32("#address-cells", 2);
        d.prop_u32("#size-cells", 2);
        d.begin("intc@8000000");
        d.prop_str("compatible", "arm,gic-v3");
        d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0x2_0000)]));
        d.prop_u32("zz-trailing", 0);
        d.end();
        d.end();
        d.blob()
    };
    let mut b = trailing();
    assert!(discover_blob(&b).is_some(), "trailing property");
    let strs = st(&b, 12);
    let strs_len = st(&b, 32);
    assert_eq!(&b[strs + strs_len - 12..strs + strs_len], b"zz-trailing\0");
    b[strs + strs_len - 1] = b'x';
    assert!(discover_blob(&b).is_none(), "unterminated property name");
}

/// The loader's kernel command line: words appended with one space, a NUL
/// left after them, and a word that does not fit refused (the loader then
/// fails, never drops it); the kernel finds `warm_its` only as a whole
/// word.
#[test]
fn kernel_command_line_options_are_never_dropped() {
    use fl4_bootinfo::{cmdline_room, has_option, CMDLINE_LEN};
    let mut c = [0u8; CMDLINE_LEN];
    let mut len = 0;
    for w in [&b"novirt"[..], b"warm_its"] {
        let at = cmdline_room(len, w.len()).expect("fits");
        if len > 0 {
            c[len] = b' ';
        }
        c[at..at + w.len()].copy_from_slice(w);
        len = at + w.len();
    }
    assert_eq!(&c[..len], b"novirt warm_its");
    assert!(has_option(&c, b"warm_its"));
    assert!(has_option(&c, b"novirt"));
    assert!(!has_option(&c, b"warm"));
    assert!(!has_option(&[0u8; CMDLINE_LEN], b"warm_its"));
    let mut other = [0u8; CMDLINE_LEN];
    other[..12].copy_from_slice(b"fb_console x");
    assert!(!has_option(&other, b"warm_its"));
    // Room for the NUL: a first word of 255 bytes fits, 256 does not; a
    // word after 250 bytes fits only with its space and the NUL.
    assert_eq!(cmdline_room(0, CMDLINE_LEN - 1), Some(0));
    assert_eq!(cmdline_room(0, CMDLINE_LEN), None);
    assert_eq!(cmdline_room(250, 4), Some(251));
    assert_eq!(cmdline_room(250, 5), None);
    assert_eq!(cmdline_room(1, usize::MAX), None);
    // A full line (no NUL) is read whole.
    let mut full = [b'x'; CMDLINE_LEN];
    full[CMDLINE_LEN - 9..].copy_from_slice(b" warm_its");
    assert!(has_option(&full, b"warm_its"));
}

/// GICv2 with a 4 KiB distributor right before an 8 KiB CPU interface
/// (arm,gic.yaml's example; RK3328's GIC-400): both kept as given.
#[test]
fn a_gicv2_distributor_can_be_one_page() {
    let regs = [(0xff81_1000u64, 0x1000u64), (0xff81_2000, 0x2000), (0xff81_4000, 0x2000), (0xff81_6000, 0x2000)];
    let h = dt_hw("arm,gic-400", &regs, None);
    assert_eq!(h.gic_version, 2);
    assert_eq!((h.gicd, h.gicd_len), (0xff81_1000, 0x1000));
    assert_eq!((h.gicc, h.gicc_len), (0xff81_2000, 0x2000));
    assert_eq!((h.gich, h.gich_len), (0xff81_4000, 0x2000));
    // What lib.rs maps and keeps of it: one page, up to the CPU interface.
    let lim = 1 << 40;
    assert_eq!(gic_layout::gicd_extent(h.gic_version, h.gicd, h.gicd_len, lim), Some((0x1000, 0x1000)));
    assert_eq!(h.gicd + 0x1000, h.gicc);
    // GICv3's is 64 KiB.
    assert_eq!(gic_layout::gicd_extent(3, 0x800_0000, 0x1000, lim), None);
    assert_eq!(gic_layout::gicd_extent(3, 0x800_0000, 0x1_0000, lim), Some((0x1_0000, 0x1_0000)));
    assert_eq!(gic_layout::gicd_extent(3, 0x800_0000, 0, lim), Some((0x1_0000, 0x1_0000)));
    // Unsized GICv2: one page; a larger size is kept whole.
    assert_eq!(gic_layout::gicd_extent(2, 0x800_0000, 0, lim), Some((0x1000, 0x1000)));
    assert_eq!(gic_layout::gicd_extent(2, 0x800_0000, 0x1_0000, lim), Some((0x1000, 0x1_0000)));
    // Off a page, at 0, past the physical address space.
    assert_eq!(gic_layout::gicd_extent(2, 0xff81_1800, 0x1000, lim), None);
    assert_eq!(gic_layout::gicd_extent(2, 0, 0x1000, lim), None);
    assert_eq!(gic_layout::gicd_extent(2, lim - 0x1000, 0x2000, lim), None);
    assert_eq!(gic_layout::gicd_extent(2, lim - 0x1000, 0x1000, lim), Some((0x1000, 0x1000)));
}

/// Whether an ITS is still set up from before the kernel: it was enabled,
/// or its command queue or a table is valid; addresses (UNKNOWN at reset)
/// tell nothing. An ITS with GITS_TYPER.INV is always taken over; one
/// without only cold, on a boot path that hands it over from reset.
#[test]
fn a_warm_its_is_told_from_a_cold_one() {
    let v = 1u64 << 63;
    let cold = [0u64; 8];
    assert!(!gic_layout::its_is_warm(false, 0, &cold));
    // Cold reset with UNKNOWN addresses in the command queue and the
    // implemented tables, none valid: cold, taken over on a trusted
    // handoff even without INV.
    let mut unknown = [0u64; 8];
    unknown[0] = 1 << 56 | 7 << 48 | 0x4_0002_0000;
    unknown[1] = 4 << 56 | 15 << 48 | 0x7_1234_5000;
    let warm = gic_layout::its_is_warm(false, 0x4_0001_0000 | 0xf, &unknown);
    assert!(!warm);
    assert!(gic_layout::its_takeover_allowed(false, warm, true));
    // The same without a trusted handoff: not without INV.
    assert!(!gic_layout::its_takeover_allowed(false, false, false));
    assert!(gic_layout::its_takeover_allowed(true, false, false));
    // Warm: enabled, a valid command queue, a valid table.
    assert!(gic_layout::its_is_warm(true, 0, &cold), "was enabled");
    assert!(gic_layout::its_is_warm(false, v, &cold), "CBASER valid");
    let mut b = cold;
    b[3] = v;
    assert!(gic_layout::its_is_warm(false, 0, &b), "BASER valid");
    // Warm needs INV, whatever the handoff.
    assert!(!gic_layout::its_takeover_allowed(false, true, true));
    assert!(gic_layout::its_takeover_allowed(true, true, false));
    assert!(gic_layout::its_takeover_allowed(true, true, true));
}

/// ITS nodes under identity buses (empty `ranges`, as QEMU's).
fn dt_its(its_regs: &[Vec<u8>]) -> hw::Hw {
    dt_its_bus(Some(&[]), Some(&[]), its_regs)
}

/// Every ITS node of a device tree is recorded with its physical address
/// (through its buses' `ranges`) and size (the larger where one is given
/// twice); one whose reg cannot be read, is empty, or does not translate
/// fails the boot rather than go to user space.
#[test]
fn every_its_of_the_device_tree_is_recorded() {
    let h = dt_hw("arm,gic-v3", &[GICD, (0x80a_0000, 0x2_0000)], None);
    assert_eq!(&h.its[..h.n_its], &[its_block(0x808_0000, 0x2_0000, 0x3_0000)]);
    assert!(!h.its_lost);
    // The same ITS twice with the same size is one; with two sizes, which
    // is right is unknown.
    let h = dt_its(&[reg2(&[(0x808_0000, 0x2_0000)]), reg2(&[(0x900_0000, 0x4_0000)]), reg2(&[(0x808_0000, 0x2_0000)])]);
    assert_eq!(&h.its[..h.n_its], &[its_block(0x808_0000, 0x2_0000, 0x3_0000), its_block(0x900_0000, 0x4_0000, 0x4_0000)]);
    assert!(!h.its_lost);
    assert!(dt_its(&[reg2(&[(0x808_0000, 0x2_0000)]), reg2(&[(0x808_0000, 0x10_0000)])]).its_lost);
    // Zero length, or less than the two frames.
    assert!(dt_its(&[reg2(&[(0x808_0000, 0)])]).its_lost);
    assert!(dt_its(&[reg2(&[(0x808_0000, 0x1_0000)])]).its_lost);
    // Not a whole number of (address, size) entries; empty.
    let mut bad = reg2(&[(0x808_0000, 0x2_0000)]);
    bad.truncate(12);
    assert!(dt_its(&[bad]).its_lost);
    assert!(dt_its(&[vec![]]).its_lost);
    // Off 64 KiB.
    assert!(dt_its(&[reg2(&[(0x808_1000, 0x2_0000)])]).its_lost);
    let many: Vec<Vec<u8>> = (0..hw::MAX_ITS as u64 + 1).map(|i| reg2(&[(0x1000_0000 + i * 0x10_0000, 0x2_0000)])).collect();
    let h = dt_its(&many);
    assert_eq!(h.n_its, hw::MAX_ITS);
    assert!(h.its_lost);

    // A bus that maps its children's 0..0x1000_0000 to 0x40_0000_0000: the
    // ITS's reg is a bus address.
    let r = [(0, 0x40_0000_0000, 0x1000_0000)];
    let h = dt_its_bus(Some(&r), Some(&[]), &[reg2(&[(0x808_0000, 0x2_0000)])]);
    assert_eq!(&h.its[..h.n_its], &[its_block(0x40_0808_0000, 0x2_0000, 0x3_0000)]);
    assert!(!h.its_lost);
    // The GIC's own registers go through the same ranges.
    assert_eq!(h.gicd, 0x40_0800_0000);
    assert_eq!(&h.gicr[..h.n_gicr], &[(0x40_080a_0000, 0x2_0000)]);
    assert!(!h.gicr_lost);
    // A bus window that ends right after the ITS's two frames: a third
    // frame would be past it, so the ITS may not grow there.
    let tight = [(0x800_0000, 0x40_0800_0000, 0xa_0000)];
    let h = dt_its_bus(Some(&tight), Some(&[]), &[reg2(&[(0x808_0000, 0x2_0000)])]);
    assert_eq!(&h.its[..h.n_its], &[its_block(0x40_0808_0000, 0x2_0000, 0x2_0000)]);
    // ... and the GIC's redistributor past the window does not translate.
    assert!(h.gicr_lost);
    // The GIC node's own non-empty ranges too.
    let gic_r: Vec<u8> = [0x800_0000u64, 0x200_0000, 0x100_0000].iter().flat_map(|w| w.to_be_bytes()).collect();
    let h = dt_its_bus(Some(&r), Some(&gic_r), &[reg2(&[(0x808_0000, 0x2_0000)])]);
    assert_eq!(&h.its[..h.n_its], &[its_block(0x40_0208_0000, 0x2_0000, 0x3_0000)]);
    assert!(!h.its_lost);
    // Outside every range, or straddling its end; a bus without ranges.
    assert!(dt_its_bus(Some(&r), Some(&[]), &[reg2(&[(0x1000_0000, 0x2_0000)])]).its_lost);
    assert!(dt_its_bus(Some(&r), Some(&[]), &[reg2(&[(0xfff_0000, 0x2_0000)])]).its_lost);
    // Without ranges on /soc nothing under it translates: not the ITS,
    // nor the GIC, so there is no hardware to boot on.
    assert!(dt_its_bus_try(None, Some(&[]), &[reg2(&[(0x808_0000, 0x2_0000)])]).is_none());
    assert!(dt_its_bus(Some(&[]), None, &[reg2(&[(0x808_0000, 0x2_0000)])]).its_lost);
}

// ---- EL0 access to per-core registers (audit F3) -------------------------------

#[test]
fn el0_debug_channel_and_trace_registers_trap() {
    // MDSCR_EL1.TDCC; no debug events (MDE, KDE, SS clear).
    assert_eq!(el0_access::MDSCR_EL1, 1 << 12);
    // CPACR_EL1.TTA in both FP/SIMD states, which differ only in FPEN.
    for v in [el0_access::CPACR_FP_EL0_TRAP, el0_access::CPACR_FP_NONE] {
        assert_ne!(v & (1 << 28), 0, "{v:#x}");
        // SVE and SME stay trapped (ZEN, SMEN clear).
        assert_eq!(v & (3 << 16 | 3 << 24), 0, "{v:#x}");
    }
    assert_eq!((el0_access::CPACR_FP_EL0_TRAP >> 20) & 3, 1);
    assert_eq!((el0_access::CPACR_FP_NONE >> 20) & 3, 3);
}

/// Audit F5: a core runs beside the boot core only with 16-bit ASIDs and
/// the boot core's access-flag management, PAN, at least its physical
/// address size, its counter frequency, and its EL2/VHE/VGIC capabilities.
#[test]
fn an_application_processor_must_match_the_boot_core() {
    let bsp = ap_caps::Caps {
        asid16: true,
        hw_af: true,
        pan: true,
        pa_bits: 40,
        cntfrq: 62_500_000,
        el2: false,
        vhe: false,
        vmid16: false,
        vgic_lrs: 0,
        virt: false,
        vgic_vtr: 0,
    };
    assert!(ap_caps::ap_matches(&bsp, &bsp));
    assert!(ap_caps::ap_matches(&bsp, &ap_caps::Caps { pa_bits: 48, ..bsp }));
    for ap in [
        ap_caps::Caps { asid16: false, ..bsp },
        ap_caps::Caps { hw_af: false, ..bsp },
        ap_caps::Caps { pan: false, ..bsp },
        ap_caps::Caps { pa_bits: 36, ..bsp },
        ap_caps::Caps { cntfrq: 24_000_000, ..bsp },
        ap_caps::Caps { cntfrq: 0, ..bsp },
        ap_caps::Caps { el2: true, ..bsp },
        ap_caps::Caps { vhe: true, ..bsp },
        ap_caps::Caps { vmid16: true, ..bsp },
        ap_caps::Caps { vgic_lrs: 4, ..bsp },
    ] {
        assert!(!ap_caps::ap_matches(&bsp, &ap), "{ap:?}");
    }
    // EL2 VHE mode: every AP must match EL2, VHE, VMID16 and VGIC LR count.
    let el2_bsp = ap_caps::Caps { el2: true, vhe: true, vmid16: true, vgic_lrs: 4, ..bsp };
    assert!(ap_caps::ap_matches(&el2_bsp, &el2_bsp));
    for ap in [
        ap_caps::Caps { el2: false, ..el2_bsp },
        ap_caps::Caps { vhe: false, ..el2_bsp },
        ap_caps::Caps { vmid16: false, ..el2_bsp },
        ap_caps::Caps { vgic_lrs: 3, ..el2_bsp },
        ap_caps::Caps { vgic_lrs: 0, ..el2_bsp },
    ] {
        assert!(!ap_caps::ap_matches(&el2_bsp, &ap), "el2 {ap:?}");
    }
    // EL2 nVHE mode: el2=true, vhe=false; APs must match nVHE and VGIC LR count.
    let nvhe_bsp = ap_caps::Caps { el2: true, vhe: false, vmid16: false, vgic_lrs: 4, ..bsp };
    assert!(ap_caps::ap_matches(&nvhe_bsp, &nvhe_bsp));
    // Heterogeneous PA (BSP 40-bit, AP 48-bit): AP is admitted and both cores
    // use the BSP's Stage-2 IPA geometry (40-bit).
    assert!(ap_caps::ap_matches(&nvhe_bsp, &ap_caps::Caps { pa_bits: 48, ..nvhe_bsp }));
    assert_eq!(vmid::s2_ipa_bits_for_pa(36), 40);
    assert_eq!(vmid::s2_ipa_bits_for_pa(40), 40);
    assert_eq!(vmid::s2_ipa_bits_for_pa(42), 40);
    assert_eq!(vmid::s2_ipa_bits_for_pa(44), 44);
    assert_eq!(vmid::s2_ipa_bits_for_pa(48), 48);
    assert_eq!(vmid::s2_ipa_bits_for_pa(52), 48);
    for ap in [
        ap_caps::Caps { el2: false, ..nvhe_bsp },
        ap_caps::Caps { vhe: true, ..nvhe_bsp },
        ap_caps::Caps { vmid16: true, ..nvhe_bsp },
        ap_caps::Caps { vgic_lrs: 3, ..nvhe_bsp },
        ap_caps::Caps { vgic_lrs: 0, ..nvhe_bsp },
    ] {
        assert!(!ap_caps::ap_matches(&nvhe_bsp, &ap), "nvhe {ap:?}");
    }
    // Nor a core with what the boot core lacks: the kernel set up neither.
    let plain = ap_caps::Caps { hw_af: false, pan: false, ..bsp };
    assert!(!ap_caps::ap_matches(&plain, &bsp));
    assert!(!ap_caps::ap_matches(&ap_caps::Caps { cntfrq: 0, ..bsp }, &ap_caps::Caps { cntfrq: 0, ..bsp }));
}

/// With guests (`virt_ready`), a core must give guests the boot core's
/// VGIC as the VCPU ABI reports it, the whole capped type register: not
/// only the list register count, but the priority and preemption bits too
/// (the active-priority registers each core saves). Without guests (EL1,
/// or EL2 with `novirt` or no usable VGIC) the type register is not
/// compared, so nothing is refused that was not before.
#[test]
fn with_guests_an_application_processor_must_give_them_the_boot_cores_vgic() {
    // ICH_VTR_EL2: PRIbits 5 (field 4), PREbits 5 (field 4), A3V, 4 list
    // registers (field 3).
    const VTR: u64 = (4 << 29) | (4 << 26) | (1 << 21) | 3;
    const PRE6: u64 = (VTR & !(7 << 26)) | (5 << 26);
    const PRI6: u64 = (VTR & !(7 << 29)) | (5 << 29);
    const NO_A3V: u64 = VTR & !(1 << 21);
    let el1 = ap_caps::Caps {
        asid16: true,
        hw_af: true,
        pan: true,
        pa_bits: 40,
        cntfrq: 62_500_000,
        el2: false,
        vhe: false,
        vmid16: false,
        vgic_lrs: 0,
        virt: false,
        vgic_vtr: 0,
    };
    for (vhe, vmid16) in [(true, true), (false, false)] {
        let host = ap_caps::Caps { el2: true, vhe, vmid16, vgic_lrs: 4, vgic_vtr: VTR, ..el1 };
        // novirt, or no usable VGIC: as before, only the list register count.
        for vtr in [VTR, PRE6, PRI6, NO_A3V, 0] {
            assert!(ap_caps::ap_matches(&host, &ap_caps::Caps { vgic_vtr: vtr, ..host }), "native {vtr:#x}");
        }
        assert!(!ap_caps::ap_matches(&host, &ap_caps::Caps { vgic_lrs: 3, ..host }));
        // Guests: the whole register.
        let guests = ap_caps::Caps { virt: true, ..host };
        assert!(ap_caps::ap_matches(&guests, &guests));
        assert!(ap_caps::ap_matches(&guests, &ap_caps::Caps { pa_bits: 48, ..guests }));
        for ap in [
            ap_caps::Caps { vgic_vtr: PRE6, ..guests },
            ap_caps::Caps { vgic_vtr: PRI6, ..guests },
            ap_caps::Caps { vgic_vtr: NO_A3V, ..guests },
            ap_caps::Caps { vgic_vtr: (VTR & !0x1f) | 2, vgic_lrs: 3, ..guests },
            ap_caps::Caps { vgic_vtr: 0, ..guests },
            ap_caps::Caps { virt: false, ..guests },
        ] {
            assert!(!ap_caps::ap_matches(&guests, &ap), "guests {ap:?}");
        }
    }
    // EL1: no VGIC at all.
    assert!(ap_caps::ap_matches(&el1, &el1));
}

#[test]
fn vmid8_single_revoke_preserves_other_asids_and_wrap_bumps_gen() {
    let tbl = vmid::VmidTable::new();
    assert_eq!(tbl.generation(), 1);
    let (va, wrapped_a) = tbl.alloc_step(10);
    let (vb, wrapped_b) = tbl.alloc_step(20);
    assert!(!wrapped_a && !wrapped_b);
    assert_eq!(tbl.lookup(10), Some(va));
    assert_eq!(tbl.lookup(20), Some(vb));

    // Single-ASID revocation releases only ASID 10's slot and does NOT bump generation:
    // ASID 20 must remain resolvable so invalidate_stage2_page(20) still finds its VMID.
    tbl.release_slot(va);
    assert_eq!(tbl.generation(), 1);
    assert_eq!(tbl.lookup(10), None);
    assert_eq!(tbl.lookup(20), Some(vb));

    // Fill remaining slots up to 255 -> no wrap yet.
    for asid in 21..(21 + 253) {
        let (_v, wrapped) = tbl.alloc_step(asid);
        assert!(!wrapped, "unexpected wrap at asid {asid}");
    }
    // The 256th allocation wraps to slot 1 -> caller bumps gen, retires cores, flushes all VMIDs, then installs.
    let (slot, wrapped) = tbl.alloc_step(500);
    assert!(wrapped);
    assert_eq!(slot, 1);
    let new_gen = tbl.bump_gen();
    assert_eq!(new_gen, 2);
    // Old-generation ASIDs are now invalidated; slot 1 is not visible until install_slot runs.
    assert_eq!(tbl.lookup(20), None);
    assert_eq!(tbl.lookup(500), None);
    tbl.install_slot(slot, new_gen, 500);
    assert_eq!(tbl.lookup(500), Some(1));
    assert_eq!(vmid::mask_lr_bitmap(0xffff_ffff, 16), 0xffff);
    assert_eq!(vmid::mask_lr_bitmap(0xffff_ffff, 4), 0xf);
}

/// Audit F5: EL2 set-up reads PMCR_EL0 only with an architected PMU, as
/// `el0_access::has_pmu` decides: PMUVer 0xf (IMPLEMENTATION DEFINED) has
/// no PMCR_EL0, and reading it there traps before any vector is set.
#[test]
fn el2_setup_reads_pmcr_only_with_an_architected_pmu() {
    let boot = include_str!("../../crates/fl4-kernel-aarch64/src/boot.S");
    let at = boot.find("mrs     x9, pmcr_el0").expect("PMCR_EL0 read");
    let before: Vec<&str> = boot[..at].lines().rev().skip(1).take(4).map(|l| l.split("/*").next().unwrap().trim()).collect();
    assert_eq!(before, ["b.eq    4f", "cmp     x9, #0xf", "cbz     x9, 4f", "ubfx    x9, x9, #8, #4"]);
}

/// The pointer authentication key registers, APIAKeyLo_EL1 to
/// APGAKeyHi_EL1.
const PAUTH_KEYS: [&str; 10] = ["s3_0_c2_c1_0", "s3_0_c2_c1_1", "s3_0_c2_c1_2", "s3_0_c2_c1_3", "s3_0_c2_c2_0",
    "s3_0_c2_c2_1", "s3_0_c2_c2_2", "s3_0_c2_c2_3", "s3_0_c2_c3_0", "s3_0_c2_c3_1"];

/// `fl4_el_setup` as written in boot.S, comments left out and labels on
/// lines of their own ("7:").
fn el_setup_lines() -> Vec<String> {
    let boot = include_str!("../../crates/fl4-kernel-aarch64/src/boot.S");
    let at = boot.find("\nfl4_el_setup:\n").expect("fl4_el_setup") + "\nfl4_el_setup:\n".len();
    let end = boot[at..].find("\n\n").map_or(boot.len(), |e| at + e);
    let mut out = vec![];
    let mut comment = false;
    for l in boot[at..end].lines() {
        let mut l = l.to_string();
        if comment {
            match l.find("*/") {
                Some(e) => { l = l[e + 2..].to_string(); comment = false; }
                None => continue,
            }
        }
        while let Some(b) = l.find("/*") {
            match l[b..].find("*/") {
                Some(e) => l.replace_range(b..b + e + 2, ""),
                None => { comment = true; l.truncate(b); }
            }
        }
        let mut l = l.trim().to_string();
        if let Some((n, rest)) = l.split_once(':') {
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                out.push(format!("{n}:"));
                l = rest.trim().to_string();
            }
        }
        if !l.is_empty() {
            out.push(l.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    out
}

/// A constant of boot.S: a number, an `.equ` name or `(a << b)`.
fn boot_const(e: &str) -> u64 {
    let e = e.trim().trim_start_matches('(').trim_end_matches(')').trim();
    if let Some((a, b)) = e.split_once("<<") {
        return boot_const(a) << boot_const(b);
    }
    if let Some(h) = e.strip_prefix("0x") {
        return u64::from_str_radix(h, 16).unwrap();
    }
    if let Ok(v) = e.parse() {
        return v;
    }
    let boot = include_str!("../../crates/fl4-kernel-aarch64/src/boot.S");
    let def = boot.lines().find_map(|l| l.strip_prefix(".equ ").and_then(|d| d.split_once(',')).filter(|(n, _)| n.trim() == e));
    boot_const(def.unwrap_or_else(|| panic!("unknown constant {e}")).1)
}

/// Run `fl4_el_setup` on a core entering it at `el` (1 or 2; at 2 with
/// HCR_EL2.E2H as `e2h`), whose ID_AA64ISAR1_EL1 and ID_AA64ISAR2_EL1 are
/// `isar1` and `isar2` and whose other ID registers read zero. Returns the
/// system registers written, with the EL each was written at and the value.
/// Panics on what would trap or be UNDEFINED: a key register access
/// without FEAT_PAuth, one at EL1 entered from EL2 before HCR_EL2.APK is
/// set, or any EL2 register access from a core entered at EL1.
fn run_el_setup(el: u64, e2h: bool, isar1: u64, isar2: u64) -> Vec<(u64, String, u64)> {
    let c = el_setup_lines();
    let entry_el = el;
    let mut el = el;
    let pauth = (isar1 & 0xff0) | (isar2 & 0xf000) != 0;
    let mut x: std::collections::HashMap<String, u64> = Default::default();
    let mut hcr = (e2h as u64) << 34;
    let mut elr = None;
    let mut flags = (0u64, 0u64);
    let mut writes = vec![];
    let label = |pc: usize, t: &str| -> usize {
        let (n, dir) = t.split_at(t.len() - 1);
        let l = format!("{n}:");
        match dir {
            "f" => pc + c[pc..].iter().position(|x| *x == l).unwrap_or_else(|| panic!("no {l} after {pc}")),
            "b" => c[..pc].iter().rposition(|x| *x == l).unwrap_or_else(|| panic!("no {l} before {pc}")),
            _ => panic!("branch to {t}"),
        }
    };
    let el2_reg = |r: &str| r.ends_with("_el2") || r.starts_with("s3_4_");
    let mut pc = 0;
    for _ in 0..10_000 {
        let l = c[pc].clone();
        pc += 1;
        if l.ends_with(':') {
            continue;
        }
        let (op, args) = l.split_once(' ').unwrap_or((&l, ""));
        let a: Vec<&str> = args.split(", ").collect();
        let get = |x: &std::collections::HashMap<String, u64>, o: &str| -> u64 {
            match o.strip_prefix('#') {
                Some(imm) => boot_const(imm),
                None if o == "xzr" => 0,
                None => *x.get(o).unwrap_or_else(|| panic!("{o} read before it is written: {l}")),
            }
        };
        match op {
            "isb" => {}
            "mrs" => {
                let r = a[1].to_ascii_lowercase();
                assert!(!(entry_el == 1 && el2_reg(&r)), "EL2 register read at EL1: {l}");
                let v = match r.as_str() {
                    "currentel" => el << 2,
                    "hcr_el2" => hcr,
                    "id_aa64isar1_el1" => isar1,
                    "s3_0_c0_c6_2" => isar2,
                    _ => 0,
                };
                x.insert(a[0].into(), v);
            }
            "msr" => {
                let r = a[0].to_ascii_lowercase();
                let v = get(&x, a[1]);
                assert!(!(entry_el == 1 && el2_reg(&r)), "EL2 register written at EL1: {l}");
                if PAUTH_KEYS.contains(&r.as_str()) {
                    assert!(pauth, "key register written without FEAT_PAuth (UNDEFINED): {l}");
                    assert!(entry_el == 1 || hcr & (1 << 40) != 0, "key written before HCR_EL2.APK: {l}");
                }
                match r.as_str() {
                    // E2H fixed at 1 (no FEAT_E2H0) stays 1.
                    "hcr_el2" => hcr = v | (e2h as u64) << 34,
                    "elr_el2" => elr = Some(v),
                    _ => {}
                }
                writes.push((el, r, v));
            }
            "mov" => { let v = get(&x, a[1]); x.insert(a[0].into(), v); }
            "ldr" => { x.insert(a[0].into(), boot_const(a[1].strip_prefix('=').expect("literal load"))); }
            "adr" => {
                let v = if a[1].ends_with('f') || a[1].ends_with('b') { 0x1_0000_0000 + label(pc, a[1]) as u64 } else { 0xadd0_0000 };
                x.insert(a[0].into(), v);
            }
            "lsr" => { let v = get(&x, a[1]) >> get(&x, a[2]); x.insert(a[0].into(), v); }
            "and" => { let v = get(&x, a[1]) & get(&x, a[2]); x.insert(a[0].into(), v); }
            "orr" => { let v = get(&x, a[1]) | get(&x, a[2]); x.insert(a[0].into(), v); }
            "ubfx" => {
                let (lsb, w) = (get(&x, a[2]), get(&x, a[3]));
                let v = (get(&x, a[1]) >> lsb) & ((1 << w) - 1);
                x.insert(a[0].into(), v);
            }
            "cmp" => flags = (get(&x, a[0]), get(&x, a[1])),
            "b.ne" | "b.eq" | "b.lo" => {
                let take = match op { "b.ne" => flags.0 != flags.1, "b.eq" => flags.0 == flags.1, _ => flags.0 < flags.1 };
                if take { pc = label(pc, a[0]); }
            }
            "b" => pc = label(pc, a[0]),
            "cbz" => if get(&x, a[0]) == 0 { pc = label(pc, a[1]); },
            "tbz" | "tbnz" => {
                let set = get(&x, a[0]) >> get(&x, a[1]) & 1 != 0;
                if set == (op == "tbnz") { pc = label(pc, a[2]); }
            }
            "eret" => {
                assert_eq!(el, 2, "eret at EL1");
                el = 1;
                pc = (elr.expect("ELR_EL2 set before eret") - 0x1_0000_0000) as usize;
            }
            "ret" => {
                assert_eq!(el, 1, "returns at EL{el}");
                return writes;
            }
            _ => panic!("the model has no {l}"),
        }
    }
    panic!("fl4_el_setup does not return");
}

/// The pointer authentication ID fields: (ISAR1 bits, ISAR2 bits) of APA,
/// API, GPA, GPI, APA3 and GPA3, each set or clear, with every other field
/// of the two registers set.
fn pauth_id_combinations() -> Vec<(u64, u64)> {
    let fields = [(1u64 << 4, 0u64), (1 << 8, 0), (1 << 24, 0), (1 << 28, 0), (0, 1 << 12), (0, 1 << 8)];
    (0..64u32).map(|m| {
        let (mut i1, mut i2) = (0x00f0_000f_u64 | 0xffff_ffff_0000_0000, 0xffff_ffff_ffff_00ff_u64);
        for (b, f) in fields.iter().enumerate() {
            if m & (1 << b) != 0 {
                i1 |= f.0;
                i2 |= f.1;
            }
        }
        (i1, i2)
    }).collect()
}

/// Audit r10 F3: every core clears all ten pointer authentication keys
/// whenever it has FEAT_PAuth (APA, API or APA3), entered at EL1 as at EL2,
/// and writes none without it (an UNDEFINED access). Before, a core entered
/// at EL1 (QEMU's default, KVM and cloud guests) skipped the clearing and
/// kept the firmware's keys, which PACGA, enabled by no SCTLR_EL1 bit, lets
/// EL0 compute with. From EL2, HCR_EL2.{APK,API} are set for any of the six
/// fields before any key is written (`run_el_setup` checks the order).
#[test]
fn pauth_keys_are_cleared_on_every_entry() {
    for (isar1, isar2) in pauth_id_combinations() {
        let feat = (isar1 & 0xff0) | (isar2 & 0xf000) != 0;
        let any = (isar1 & 0xff00_0ff0) | (isar2 & 0xff00) != 0;
        for (el, e2h) in [(1, false), (2, false), (2, true)] {
            let w = run_el_setup(el, e2h, isar1, isar2);
            let keys: Vec<&(u64, String, u64)> = w.iter().filter(|(_, r, _)| PAUTH_KEYS.contains(&r.as_str())).collect();
            let what = format!("EL{el} e2h {e2h} isar1 {isar1:#x} isar2 {isar2:#x}");
            if feat {
                assert_eq!(keys.iter().map(|k| k.1.as_str()).collect::<Vec<_>>(), PAUTH_KEYS, "{what}");
                assert!(keys.iter().all(|k| k.0 == 1 && k.2 == 0), "{what}");
            } else {
                assert!(keys.is_empty(), "{what}");
            }
            if el == 2 {
                let hcr: Vec<u64> = w.iter().filter(|(_, r, _)| r == "hcr_el2").map(|w| w.2).collect();
                assert_eq!(hcr, [1 << 31 | if any { 3 << 40 } else { 0 }], "{what}");
            }
        }
    }
}

/// The code of `label` in boot.S, up to the next blank line, comments and
/// labels' own lines left out.
fn boot_code(label: &str) -> Vec<String> {
    let boot = include_str!("../../crates/fl4-kernel-aarch64/src/boot.S");
    let at = boot.find(&format!("\n{label}:\n")).unwrap_or_else(|| panic!("{label} not found")) + label.len() + 3;
    let end = boot[at..].find("\n\n").map_or(boot.len(), |e| at + e);
    let mut out = vec![];
    let mut comment = false;
    for l in boot[at..end].lines() {
        let mut l = l.to_string();
        if comment {
            match l.find("*/") {
                Some(e) => { l = l[e + 2..].to_string(); comment = false; }
                None => continue,
            }
        }
        if let Some(b) = l.find("/*") {
            comment = !l[b..].contains("*/");
            l.truncate(b);
        }
        let l = l.trim();
        let l = match l.split_once(':') { Some((n, rest)) if n.chars().all(|c| c.is_ascii_digit()) => rest.trim(), _ => l };
        if !l.is_empty() {
            out.push(l.to_string());
        }
    }
    out
}

/// Audit r7 F1: at EL2, the first thing `fl4_el_setup` does is point
/// VBAR_EL2 at FL4's own vectors, before any register access that could
/// trap (the firmware's VBAR_EL2 lies in memory that becomes user RAM).
/// With pointer authentication HCR_EL2 gets API and APK (PACGA has no
/// SCTLR_EL1 enable: with API clear it traps from EL0 to EL2) and every key
/// is cleared; the fine-grained trap registers and HCRX_EL2 are cleared
/// when implemented. Audit r9 F1: the keys only with FEAT_PAuth (APA, API
/// or APA3), the one condition under which any key register exists.
#[test]
fn el2_setup_leaves_nothing_trapping_to_el2() {
    let c = boot_code("fl4_el_setup");
    let el2 = c.iter().position(|l| l == "b.ne    1f").expect("EL check");
    assert_eq!(&c[el2 + 1..el2 + 4], ["adr     x9, fl4_el2_vectors", "msr     vbar_el2, x9", "isb"]);
    let has = |l: &str| c.iter().any(|x| x == l);
    assert!(has("orr     x9, x9, #(3 << 40)"));
    // HCR_EL2.{APK,API} and the keys for each combination of the pointer
    // authentication ID fields: `pauth_keys_are_cleared_on_every_entry`.
    for r in ["s3_4_c1_c1_4", "s3_4_c1_c1_5", "s3_4_c1_c1_6", "s3_4_c3_c1_4", "s3_4_c3_c1_5", "s3_4_c3_c1_6",
        "s3_4_c3_c1_0", "s3_4_c3_c1_1", "s3_4_c3_c1_2", "s3_4_c3_c1_3", "s3_4_c3_c1_7", "s3_4_c1_c2_2"] {
        assert!(has(&format!("msr     {r}, xzr")), "{r} not cleared");
    }
    // FEAT_FGT, FEAT_HCX and FEAT_AMU fields of the ID registers.
    assert!(has("ubfx    x10, x10, #56, #4") && has("ubfx    x10, x10, #40, #4") && has("ubfx    x11, x11, #44, #4"));
}

/// Audit r7 F1 (r8 F1 for E2H fixed at 1): FL4's EL2 vectors pass a synchronous exception from AArch64
/// EL0 on to EL1 as an Undefined Instruction exception, and stop the core
/// on anything else; nothing at EL2 runs outside the kernel image.
#[test]
fn el2_vectors_reflect_el0_traps_and_halt_on_the_rest() {
    let boot = include_str!("../../crates/fl4-kernel-aarch64/src/boot.S");
    let at = boot.find("fl4_el2_vectors:").unwrap();
    assert_eq!(boot[..at].lines().next_back().unwrap().trim(), ".balign 2048");
    let v = boot_code("fl4_el2_vectors");
    assert_eq!(v, [".rept 8", "el2_halt", ".endr", ".balign 128", "b       fl4_el2_reflect", ".rept 7", "el2_halt", ".endr"]);
    assert!(boot.contains(".macro el2_halt\n    .balign 128\n    b       fl4_el2_halt\n.endm"));
    let r = boot_code("fl4_el2_reflect");
    assert_eq!(&r[..4], ["msr     tpidr_el2, x0", "mrs     x0, spsr_el2", "tst     x0, #0x1f", "b.ne    fl4_el2_halt"]);
    // The reflection itself, per HCR_EL2.E2H, is run by aarch64_vhe.rs.
    assert_eq!(&r[4..], [
        "mrs     x0, hcr_el2", "tbnz    x0, #34, 13f",
        "el2_reflect_to sctlr_el1, spsr_el1, elr_el1, esr_el1, vbar_el1",
        "el2_reflect_to s3_5_c1_c0_0, s3_5_c4_c0_0, s3_5_c4_c0_1, s3_5_c5_c2_0, s3_5_c12_c0_0",
    ]);
    assert_eq!(boot_code("fl4_el2_halt"), ["wfi", "b       fl4_el2_halt"]);
}

#[test]
fn pmuserenr_is_written_only_with_a_pmu() {
    // PMUVer: neither absent nor IMPLEMENTATION DEFINED.
    assert!(!el0_access::has_pmu(0));
    assert!(!el0_access::has_pmu(0xf << 8));
    assert!(el0_access::has_pmu(4 << 8));
}

/// An interrupt from EL0 records the thread's restart PC (FAULT_IP) before
/// it waits for the serial gate: waiting services remote calls, a stall
/// among them (`fl4_core::smp::stall`), whose acknowledgement lets the
/// calling core restart, rewrite (TCB_WriteRegisters) or delete the thread
/// and retype its memory at once. After `run_entry` starts, `fl4_entry`
/// must not touch the thread it read before (x86_64 entry.rs likewise
/// saves the user state before `service_remote`); it only compares it with
/// the current thread.
#[test]
fn an_el0_interrupt_saves_the_restart_pc_before_it_can_be_stalled() {
    let entry = include_str!("../../crates/fl4-kernel-aarch64/src/entry.rs");
    let code: Vec<&str> = entry.lines().map(|l| l.split("//").next().unwrap()).collect();
    let start = code.iter().position(|l| l.contains("fn fl4_entry(")).expect("fl4_entry");
    let irq = start + code[start..].iter().position(|l| l.trim() == "if kind == KIND_IRQ {").expect("IRQ branch");
    let end = irq + code[irq..].iter().position(|l| l.trim() == "exit()").expect("end of the IRQ branch");
    let branch = &code[start..=end];
    let take = branch.iter().position(|l| l.contains("run_entry(true")).expect("run_entry in fl4_entry");
    let save = branch.iter().position(|l| l.contains("t.set_reg(s, reg::FAULT_IP, elr)")).expect("FAULT_IP save");
    assert!(save < take, "fl4_entry takes the interrupt (and services stalls) before it saves FAULT_IP");
    for l in &branch[take..] {
        let uses_t = l.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).any(|w| w.starts_with("t."));
        assert!(!uses_t, "fl4_entry touches the interrupted thread after take_irq: {}", l.trim());
    }
}

/// Before a thread running a VM (`ARMVCPURun`) goes back to its guest,
/// `exit` checks its bound notification (`fl4_core::vcpu::check`), which
/// other cores signal, unbind and delete under the serial gate: on several
/// cores it takes the gate first when it does not hold it (after a local
/// path), then starts over with the thread current then, and checks before
/// it gives the gate up. Nothing reads the notification after `unlock`.
#[test]
fn a_running_vm_checks_its_notification_under_the_gate() {
    let entry = include_str!("../../crates/fl4-kernel-aarch64/src/entry.rs");
    let code: Vec<&str> = entry.lines().map(|l| l.split("//").next().unwrap().trim()).filter(|l| !l.is_empty()).collect();
    let start = code.iter().position(|l| *l == "pub fn exit() -> ! {").expect("exit");
    let end = start + code[start..].iter().position(|l| *l == "unsafe { fl4_restore_user(ct.0 + tl::REGS) }").expect("end of exit");
    let body = &code[start..=end];
    let at = |f: &dyn Fn(&str) -> bool, what: &str| body.iter().position(|l| f(l)).unwrap_or_else(|| panic!("{what} missing from exit"));
    let ct = at(&|l| l.contains("sched::cur_thread(s)"), "the current thread");
    // Not holding the gate with a thread running a VM: take it, start over.
    let cond = at(&|l| l.contains("TS_RUNNING_VM") && l.contains("vspace_stale"), "the gate condition");
    assert!(body[..cond].iter().any(|l| l.contains("gate_held")), "the condition is about holding the gate");
    let lock = cond + body[cond..].iter().position(|l| l.contains("lock()")).expect("lock");
    assert!(body[lock + 1].contains("continue"), "starts over after taking the gate");
    let check = at(&|l| l.contains("vcpu::check(") && l.contains("ct)"), "the check");
    let unlock = at(&|l| l.contains("unlock()"), "unlock");
    assert!(ct < cond && lock < check && check < unlock, "the check runs with the gate held, before it is given up");
    assert_eq!(body.iter().filter(|l| l.contains("sched::cur_thread(")).count(), 1, "one current thread per pass");
    for l in &body[unlock..] {
        assert!(!l.contains("bound_ntfn") && !l.contains("NTFN_") && !l.contains("vcpu::check"), "exit reads the notification outside the gate: {l}");
    }
}

// ---- memory lists -----------------------------------------------------------------

#[test]
fn a_full_range_list_reports_what_it_lost() {
    let mut r = bootmem::Ranges::new();
    for i in 0..bootmem::MAX_RANGES as u64 {
        r.add(i * 0x2000, i * 0x2000 + 0x1000);
    }
    assert!(!r.lost);
    assert_eq!(r.n, bootmem::MAX_RANGES);
    // One more disjoint range, at the top where fl4-efi puts the root task.
    r.add(1 << 40, (1 << 40) + 0x1000);
    assert!(r.lost);
    // And a split by `remove` of a full list.
    let mut r2 = bootmem::Ranges::new();
    for i in 0..bootmem::MAX_RANGES as u64 {
        r2.add(i * 0x4000, i * 0x4000 + 0x3000);
    }
    r2.remove(0x1000, 0x2000);
    assert!(r2.lost);
    // Every entry of the loader's largest memory map fits.
    assert!(bootmem::MAX_RANGES >= fl4_bootinfo::MAX_MMAP + 16);
}

#[test]
fn memory_map_entries_never_wrap() {
    assert_eq!(bootmem::entry_pages(0x1000, 0x3000, u64::MAX), Some((0x1000, 0x4000)));
    assert_eq!(bootmem::entry_pages(0x1800, 0x3000, u64::MAX), Some((0x2000, 0x4000)));
    assert_eq!(bootmem::entry_pages(0x1000, 0x3000, 0x2000), Some((0x1000, 0x2000)));
    assert_eq!(bootmem::entry_pages(u64::MAX - 0xfff, 0x2000, u64::MAX), None);
    assert_eq!(bootmem::entry_pages(0xffff_ffff_ffff_f001, 0x10, u64::MAX), None);
    assert_eq!(bootmem::entry_pages(0x1000, 0, u64::MAX), None);
}

/// The kernel's device area (paging.rs): from the top of the window
/// (fl4-core aarch64 `PPTR_TOP`) to the kernel image (`KERNEL_VBASE`).
const DEVICE_BASE: u64 = 0xffff_ff80_0000_0000;
const DEVICE_TOP: u64 = fl4_bootinfo::KERNEL_VBASE;

/// Audit r10 F1: a device mapping never wraps past 2^64 into the kernel
/// image. `device_pages` lets through any range below the physical address
/// top (up to 2^48), but from DEVICE_BASE only 512 GiB remain to 2^64:
/// before, `va + size` of a 512 GiB GICR range wrapped to 0, passed the
/// `<= DEVICE_TOP` check, and `map` remapped the kernel's own text and data
/// as device memory.
#[test]
fn device_mappings_never_wrap_past_the_device_area() {
    let paging = include_str!("../../crates/fl4-kernel-aarch64/src/paging.rs");
    assert!(paging.contains("const DEVICE_BASE: u64 = PPTR_TOP;") && paging.contains("const DEVICE_TOP: u64 = KERNEL_VBASE;"));
    assert!(paging.contains("crate::bootmem::device_va(s.next_device, size, DEVICE_TOP)"));
    let va = bootmem::device_va;
    // 512 GiB from the base ends exactly at 2^64; one page less than what
    // remains to 2^64; and everything to 2^64.
    assert_eq!(va(DEVICE_BASE, 0x80_0000_0000, DEVICE_TOP), None);
    assert_eq!(va(DEVICE_BASE, u64::MAX - DEVICE_BASE + 1, DEVICE_TOP), None);
    assert_eq!(va(DEVICE_BASE, u64::MAX - DEVICE_BASE, DEVICE_TOP), None);
    assert_eq!(va(DEVICE_BASE, 1 << 48, DEVICE_TOP), None);
    for size in [0x80_0000_1000, 0xff_ffff_f000, (1 << 48) - 0x1000] {
        assert_eq!(va(DEVICE_BASE + 0x1_0000, size, DEVICE_TOP), None, "{size:#x}");
    }
    // The whole area, and one page more.
    assert_eq!(va(DEVICE_BASE, DEVICE_TOP - DEVICE_BASE, DEVICE_TOP), Some((DEVICE_BASE, DEVICE_TOP)));
    assert_eq!(va(DEVICE_BASE, DEVICE_TOP - DEVICE_BASE + 0x1000, DEVICE_TOP), None);
    // Rounding up to 64 KiB near the top: what is left after it.
    let next = DEVICE_TOP - 0x2_0000 + 0x1000;
    assert_eq!(va(next, 0x1_0000, DEVICE_TOP), Some((DEVICE_TOP - 0x1_0000, DEVICE_TOP)));
    assert_eq!(va(next, 0x1_1000, DEVICE_TOP), None);
    assert_eq!(va(DEVICE_TOP, 0x1000, DEVICE_TOP), None);
    // A `next` the rounding would wrap.
    assert_eq!(va(u64::MAX - 0x1000, 0x1000, u64::MAX), None);
    // Ordinary mappings: 64 KiB aligned, each after the last.
    assert_eq!(va(DEVICE_BASE, 0x1000, DEVICE_TOP), Some((DEVICE_BASE, DEVICE_BASE + 0x1000)));
    assert_eq!(va(DEVICE_BASE + 0x1000, 0x2_0000, DEVICE_TOP), Some((DEVICE_BASE + 0x1_0000, DEVICE_BASE + 0x3_0000)));
    // Whatever was asked, an accepted mapping lies inside the area.
    for next in [DEVICE_BASE, DEVICE_BASE + 0x1234_5000, DEVICE_TOP - 0x10_0000, DEVICE_TOP] {
        for shift in 12..64 {
            for size in [1u64 << shift, (1u64 << shift) - 0x1000, (1u64 << shift).wrapping_add(0x1000)] {
                if let Some((v, end)) = va(next, size, DEVICE_TOP) {
                    assert!(v >= next && v % 0x1_0000 == 0 && end == v + size && end <= DEVICE_TOP, "{next:#x} {size:#x}");
                }
            }
        }
    }
}

/// Audit r7 F3: the console UART's pages kept from user space are the
/// pages `map_device` maps for it, a base inside a page included (two
/// pages); before, only the first page was kept and the second went to
/// user space as a device untyped the kernel also mapped.
#[test]
fn the_consoles_kept_pages_are_the_ones_it_maps() {
    let lib = include_str!("../../crates/fl4-kernel-aarch64/src/lib.rs");
    assert!(lib.contains("paging::map_device(h.uart_base, UART_MAP)"));
    assert!(lib.contains("keep(&mut dev_used, h.uart_base, h.uart_base.saturating_add(UART_MAP));"));
    assert!(lib.contains("const UART_MAP: u64 = 0x1000;"));
    assert!(!lib.contains("dev_used.add("), "a kernel device range kept other than by `keep`");
    let top = 1 << 40;
    for base in [0x900_0000u64, 0x900_0800, 0x900_0ffc, 0x900_0001, 0xfe21_5040] {
        let (pa, size) = bootmem::device_pages(base, 0x1000, top).unwrap();
        assert_eq!(bootmem::pages_covering(base, base + 0x1000), (pa, pa + size), "{base:#x}");
    }
    assert_eq!(bootmem::pages_covering(0x900_0800, 0x900_1800), (0x900_0000, 0x900_2000));
}

/// One EFI memory descriptor of `stride` bytes.
fn efi_desc(stride: usize, ty: u32, base: u64, pages: u64, attr: u64) -> Vec<u8> {
    let mut d = vec![0u8; stride];
    d[0..4].copy_from_slice(&ty.to_le_bytes());
    d[8..16].copy_from_slice(&base.to_le_bytes());
    d[24..32].copy_from_slice(&pages.to_le_bytes());
    d[32..40].copy_from_slice(&attr.to_le_bytes());
    d
}

/// Audit r7 F4: the AArch64 loader takes only a map of whole descriptors
/// (stride at least 40 bytes, size a multiple of it, as the x86_64 loader
/// requires), and no descriptor whose size or end passes 2^64: a wrapped
/// length would shrink a RAM region and the kernel would hand the rest out
/// as device memory. Before, stride 0 or below 40 read past the
/// descriptor, and `pages * PAGE` wrapped.
#[test]
fn the_efi_memory_map_is_whole_and_never_wraps() {
    use fl4_bootinfo::{mem, MemRegion};
    let mut out = [MemRegion::default(); 4];
    let wb = 8;
    // Merged: two FREE kinds back to back, then firmware memory.
    let mut map = efi_desc(48, 7, 0x4000_0000, 0x100, wb);
    map.extend(efi_desc(48, 4, 0x4010_0000, 0x10, wb));
    map.extend(efi_desc(48, 5, 0x4011_0000, 1, wb));
    assert_eq!(efi_mmap::regions(&map, 48, &mut out), Some(2));
    assert_eq!((out[0].base, out[0].len, out[0].kind, out[0].cacheable), (0x4000_0000, 0x110_000, mem::FREE, 1));
    assert_eq!((out[1].base, out[1].len, out[1].kind), (0x4011_0000, 0x1000, mem::FIRMWARE));
    // Not contiguous, or not of one cacheability: not merged.
    let mut map = efi_desc(40, 7, 0x4000_0000, 1, wb);
    map.extend(efi_desc(40, 7, 0x4000_2000, 1, wb));
    map.extend(efi_desc(40, 7, 0x4000_3000, 1, 0));
    assert_eq!(efi_mmap::regions(&map, 40, &mut out), Some(3));
    // Layout: a stride of 0, below 40, or not dividing the size.
    let one = efi_desc(48, 7, 0x4000_0000, 1, wb);
    assert_eq!(efi_mmap::regions(&one, 0, &mut out), None);
    assert_eq!(efi_mmap::regions(&one[..32], 32, &mut out), None);
    assert_eq!(efi_mmap::regions(&one[..47], 48, &mut out), None);
    assert_eq!(efi_mmap::regions(&one, 40, &mut out), None);
    assert_eq!(efi_mmap::regions(&[], 48, &mut out), Some(0));
    assert!(efi_mmap::layout_ok(96, 48) && !efi_mmap::layout_ok(96, 36) && !efi_mmap::layout_ok(100, 48));
    // Size or end past 2^64.
    assert_eq!(efi_mmap::regions(&efi_desc(48, 7, 0, 1 << 52, wb), 48, &mut out), None);
    assert_eq!(efi_mmap::regions(&efi_desc(48, 7, 0x1000, u64::MAX / 0x1000 + 1, wb), 48, &mut out), None);
    assert_eq!(efi_mmap::regions(&efi_desc(48, 7, u64::MAX - 0xfff, 2, wb), 48, &mut out), None);
    assert_eq!(efi_mmap::regions(&efi_desc(48, 7, u64::MAX - 0xfff, 1, wb), 48, &mut out), None);
    assert_eq!(efi_mmap::regions(&efi_desc(48, 7, u64::MAX - 0x1fff, 1, wb), 48, &mut out), Some(1));
    // More regions than the record holds.
    let mut map = vec![];
    for i in 0..5u64 {
        map.extend(efi_desc(40, 7, i * 0x2000, 1, wb));
    }
    assert_eq!(efi_mmap::regions(&map, 40, &mut out), None);
    assert_eq!(efi_mmap::regions(&map[..160], 40, &mut out), Some(4));
}

#[test]
fn device_mappings_stay_below_the_physical_address_size() {
    let top = 1 << 40;
    assert_eq!(bootmem::device_pages(0x900_0000, 0x1000, top), Some((0x900_0000, 0x1000)));
    assert_eq!(bootmem::device_pages(0x900_0800, 0x1000, top), Some((0x900_0000, 0x2000)));
    assert_eq!(bootmem::device_pages(top - 0x1000, 0x1000, top), Some((top - 0x1000, 0x1000)));
    assert_eq!(bootmem::device_pages(top - 0x1000, 0x1001, top), None);
    assert_eq!(bootmem::device_pages(1 << 52, 0x1000, top), None);
    assert_eq!(bootmem::device_pages(u64::MAX - 0xff, 0x1000, u64::MAX), None);
    assert_eq!(bootmem::device_pages(0x900_0000, 0, top), None);
}

// ---- ACPI -------------------------------------------------------------------------

fn checksum(table: &mut [u8], offset: usize) {
    table[offset] = 0;
    table[offset] = table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)).wrapping_neg();
}

/// A MADT GICC entry (type 0x0b, 80 bytes): enabled core `mpidr` with its
/// redistributor at `gicr`.
fn gicc(mpidr: u64, gicr: u64) -> Vec<u8> {
    let mut e = vec![0u8; 80];
    e[0] = 0x0b;
    e[1] = 80;
    e[12..16].copy_from_slice(&1u32.to_le_bytes());
    e[60..68].copy_from_slice(&gicr.to_le_bytes());
    e[68..76].copy_from_slice(&mpidr.to_le_bytes());
    e
}

/// A GICD entry (type 0x0c, 24 bytes), GICv3.
fn gicd() -> Vec<u8> {
    let mut e = vec![0u8; 24];
    e[0] = 0x0c;
    e[1] = 24;
    e[8..16].copy_from_slice(&0x800_0000u64.to_le_bytes());
    e[20] = 3;
    e
}

/// A GICR entry (type 0x0e, 16 bytes).
fn gicr(base: u64, len: u32) -> Vec<u8> {
    let mut e = vec![0u8; 16];
    e[0] = 0x0e;
    e[1] = 16;
    e[4..12].copy_from_slice(&base.to_le_bytes());
    e[12..16].copy_from_slice(&len.to_le_bytes());
    e
}

/// Where the tables are in physical memory.
const BASE: u64 = 0x1000;

struct Tables {
    mem: Vec<u8>,
}

impl Tables {
    fn rsdp(&self) -> u64 {
        layout::MEMORY.set((self.mem.as_ptr() as u64, self.mem.len() as u64));
        BASE
    }
    fn buf(&mut self) -> &mut [u8] {
        &mut self.mem[BASE as usize..]
    }
}

/// RSDP (at 0, revision `rev`), XSDT or RSDT (at 64) and a MADT of
/// `entries` (at 128), from physical `BASE` on.
fn tables(entries: &[u8], rev: u8, root_sig: &[u8; 4]) -> Tables {
    let mut mem = vec![0u8; BASE as usize + 128 + 44 + entries.len()];
    let buf = &mut mem[BASE as usize..];
    let base = BASE;
    let madt = &mut buf[128..];
    madt[44..].copy_from_slice(entries);
    let len = madt.len() as u32;
    madt[..4].copy_from_slice(b"APIC");
    madt[4..8].copy_from_slice(&len.to_le_bytes());
    checksum(madt, 9);
    let xsdt = root_sig == b"XSDT";
    let root_len = if xsdt { 44 } else { 40 };
    let root = &mut buf[64..64 + root_len];
    root[..4].copy_from_slice(root_sig);
    root[4..8].copy_from_slice(&(root_len as u32).to_le_bytes());
    if xsdt {
        root[36..44].copy_from_slice(&(base + 128).to_le_bytes());
    } else {
        root[36..40].copy_from_slice(&((base + 128) as u32).to_le_bytes());
    }
    checksum(root, 9);
    let rsdp = &mut buf[..36];
    rsdp[..8].copy_from_slice(b"RSD PTR ");
    rsdp[15] = rev;
    if rev >= 2 {
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[24..32].copy_from_slice(&(base + 64).to_le_bytes());
    } else {
        rsdp[16..20].copy_from_slice(&((base + 64) as u32).to_le_bytes());
    }
    checksum(&mut rsdp[..20], 8);
    checksum(rsdp, 32);
    Tables { mem }
}

fn madt(entries: &[Vec<u8>]) -> Vec<u8> {
    entries.concat()
}

#[test]
fn every_cores_redistributor_is_kept_from_user_space() {
    // Sixteen cores, each with its redistributor in its GICC entry: all
    // sixteen ranges are recorded, so the kernel reserves all of them.
    let mut e = vec![gicd()];
    for c in 0..16u64 {
        e.push(gicc(c, 0x80a_0000 + c * 0x2_0000));
    }
    let t = tables(&madt(&e), 2, b"XSDT");
    let h = hw::discover(t.rsdp(), 0).expect("ACPI");
    assert_eq!(h.n_gicr, 16);
    assert!(!h.gicr_lost);
    for c in 0..16u64 {
        assert!(h.gicr[..h.n_gicr].contains(&(0x80a_0000 + c * 0x2_0000, 0x2_0000)), "core {c}");
    }
}

#[test]
fn redistributors_past_the_table_are_a_boot_failure() {
    let mut e = vec![gicd()];
    for c in 0..hw::MAX_GICR as u64 + 1 {
        e.push(gicr(0x1000_0000 + c * 0x10_0000, 0x2_0000));
    }
    let t = tables(&madt(&e), 2, b"XSDT");
    let h = hw::discover(t.rsdp(), 0).expect("ACPI");
    assert_eq!(h.n_gicr, hw::MAX_GICR);
    assert!(h.gicr_lost);
}

/// Audit round 5 F3: a MADT giving both a GICR discovery range and its
/// cores' GICC redistributors, in either order, records the discovery
/// range whole, once; the kernel then keeps all of it from user space.
#[test]
fn a_discovery_range_and_its_cores_redistributors_merge() {
    let b = 0x80a_0000u64;
    let len = 0xf6_0000u32;
    let cores = 100u64;
    let mut gicc_first = vec![gicd()];
    for c in 0..cores {
        gicc_first.push(gicc(c, b + (c % 0x7b) * 0x2_0000));
    }
    gicc_first.push(gicr(b, len));
    let mut gicr_first = vec![gicd(), gicr(b, len)];
    gicr_first.extend_from_slice(&gicc_first[1..cores as usize + 1]);
    for (name, e) in [("GICC first", gicc_first), ("GICR first", gicr_first)] {
        let t = tables(&madt(&e), 2, b"XSDT");
        let h = hw::discover(t.rsdp(), 0).expect("ACPI");
        assert!(!h.gicr_lost, "{name}");
        assert_eq!(&h.gicr[..h.n_gicr], &[(b, len as u64)], "{name}");
        // What boot reserves of it (`gic_layout::reserve`, as lib.rs runs
        // it on the recorded range): the whole discovery range.
        let last = b + len as u64 - 0x2_0000;
        let typer = |a: u64| if a == last { LAST } else { 0 };
        assert_eq!(gic_layout::reserve(b, len as u64, len as u64, typer), Some(len as u64), "{name}");
    }
}

#[test]
fn redistributor_ranges_merge_only_where_they_overlap() {
    let mut r = [(0u64, 0u64); 4];
    let mut n = 0;
    let add = |r: &mut [(u64, u64)], n: &mut usize, b, l| gic_layout::add_redistributors(r, n, b, l);
    // Adjacent ranges stay apart; an overlapping one joins both.
    assert!(add(&mut r, &mut n, 0x10_0000, 0x2_0000));
    assert!(add(&mut r, &mut n, 0x12_0000, 0x2_0000));
    assert_eq!(&r[..n], &[(0x10_0000, 0x2_0000), (0x12_0000, 0x2_0000)]);
    assert!(add(&mut r, &mut n, 0x11_0000, 0x2_0000));
    assert_eq!(&r[..n], &[(0x10_0000, 0x4_0000)]);
    // Contained: nothing new.
    assert!(add(&mut r, &mut n, 0x10_0000, 0x2_0000));
    assert_eq!(&r[..n], &[(0x10_0000, 0x4_0000)]);
    // A wider range from below swallows it.
    assert!(add(&mut r, &mut n, 0xf_0000, 0x10_0000));
    assert_eq!(&r[..n], &[(0xf_0000, 0x10_0000)]);
    // Unbounded ranges are deduplicated, never merged.
    assert!(add(&mut r, &mut n, 0xf_0000, 0));
    assert!(add(&mut r, &mut n, 0xf_0000, 0));
    assert_eq!(&r[..n], &[(0xf_0000, 0x10_0000), (0xf_0000, 0)]);
    // Address 0 and an end past the address space are not recorded.
    assert!(!add(&mut r, &mut n, 0, 0x2_0000));
    assert!(!add(&mut r, &mut n, u64::MAX - 0xffff, 0x2_0000));
    assert_eq!(n, 2);
    // Full: a range that overlaps still merges, a new one does not fit.
    assert!(add(&mut r, &mut n, 0x100_0000, 0x2_0000));
    assert!(add(&mut r, &mut n, 0x200_0000, 0x2_0000));
    assert!(add(&mut r, &mut n, 0x200_0000, 0x4_0000));
    assert!(!add(&mut r, &mut n, 0x300_0000, 0x2_0000));
    assert_eq!(n, 4);
}

#[test]
fn acpi_1_0_rsdps_use_the_rsdt_and_copy_20_bytes() {
    let e = madt(&[gicd(), gicc(0, 0x80a_0000)]);
    let mut t = tables(&e, 0, b"RSDT");
    let h = hw::discover(t.rsdp(), 0).expect("ACPI 1.0");
    assert_eq!(h.n_gicr, 1);
    assert!(h.rsdp[20..].iter().all(|&b| b == 0));
    assert_eq!(&h.rsdp[..20], &t.buf()[..20]);
}

#[test]
fn rsdp_extended_checksum_and_root_signature_are_checked() {
    let e = madt(&[gicd(), gicc(0, 0x80a_0000)]);
    // A revision 2 RSDP whose extended checksum fails: only its ACPI 1.0
    // part is used, and that points at no RSDT.
    let mut t = tables(&e, 2, b"XSDT");
    t.buf()[32] ^= 0xff;
    assert!(hw::discover(t.rsdp(), 0).is_none());
    // An XSDT pointer to something that is not an XSDT.
    let mut t = tables(&e, 2, b"XSDT");
    t.buf()[64..68].copy_from_slice(b"FACP");
    checksum(&mut t.buf()[64..108], 9);
    assert!(hw::discover(t.rsdp(), 0).is_none());
    // Both sound.
    let mut t = tables(&e, 2, b"XSDT");
    let h = hw::discover(t.rsdp(), 0).expect("ACPI 2.0");
    assert_eq!(&h.rsdp[..], &t.buf()[..36]);
}

// ---- device tree (audit F5) -----------------------------------------------------

/// A flattened device tree under construction.
#[derive(Default)]
struct Dtb {
    structs: Vec<u8>,
    strings: Vec<u8>,
}

impl Dtb {
    fn word(&mut self, w: u32) {
        self.structs.extend_from_slice(&w.to_be_bytes());
    }
    fn pad(&mut self) {
        while self.structs.len() % 4 != 0 {
            self.structs.push(0);
        }
    }
    fn begin(&mut self, name: &str) {
        self.word(1);
        self.structs.extend_from_slice(name.as_bytes());
        self.structs.push(0);
        self.pad();
    }
    fn end(&mut self) {
        self.word(2);
    }
    fn prop(&mut self, name: &str, val: &[u8]) {
        let off = self.strings.len() as u32;
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        self.word(3);
        self.word(val.len() as u32);
        self.word(off);
        self.structs.extend_from_slice(val);
        self.pad();
    }
    fn prop_u32(&mut self, name: &str, v: u32) {
        self.prop(name, &v.to_be_bytes());
    }
    fn prop_str(&mut self, name: &str, v: &str) {
        let mut b = v.as_bytes().to_vec();
        b.push(0);
        self.prop(name, &b);
    }
    /// The blob: header, an empty reservation map, then the blocks.
    fn blob(mut self) -> Vec<u8> {
        self.word(9);
        let rsv = 40u32;
        let st = rsv + 16;
        let strs = st + self.structs.len() as u32;
        let total = strs + self.strings.len() as u32;
        let mut b = Vec::new();
        for w in [0xd00d_feed, total, st, strs, rsv, 17, 16, 0, self.strings.len() as u32, self.structs.len() as u32] {
            b.extend_from_slice(&u32::to_be_bytes(w));
        }
        b.extend_from_slice(&[0; 16]);
        b.extend_from_slice(&self.structs);
        b.extend_from_slice(&self.strings);
        b
    }
}

/// `reg` of (address, size) pairs, two cells each.
fn reg2(regs: &[(u64, u64)]) -> Vec<u8> {
    regs.iter().flat_map(|&(a, s)| [a.to_be_bytes(), s.to_be_bytes()].concat()).collect()
}

/// The hardware a device tree with one interrupt controller node
/// (`compat`, `reg`, `#redistributor-regions` if given) describes.
fn dt_hw(compat: &str, regs: &[(u64, u64)], redist: Option<u32>) -> hw::Hw {
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("intc@8000000");
    d.prop_str("compatible", compat);
    d.prop_u32("#interrupt-cells", 3);
    // The cells of its children's reg (QEMU's virt board's, as the GICv3
    // binding asks for with ITS children).
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.prop("ranges", &[]);
    // reg before #redistributor-regions: a node's facts are used at its end.
    d.prop("reg", &reg2(regs));
    if let Some(n) = redist {
        d.prop_u32("#redistributor-regions", n);
    }
    // A child (an ITS) with its own reg, which is not the GIC's.
    d.begin("msi-controller@8080000");
    d.prop_str("compatible", "arm,gic-v3-its");
    d.prop("reg", &reg2(&[(0x808_0000, 0x2_0000)]));
    d.end();
    d.end();
    d.end();
    let blob = d.blob();
    let mut mem = vec![0u8; BASE as usize + blob.len()];
    mem[BASE as usize..].copy_from_slice(&blob);
    layout::MEMORY.set((mem.as_ptr() as u64, mem.len() as u64));
    let h = hw::discover(0, BASE).expect("device tree");
    layout::MEMORY.set((0, 0));
    h
}

const GICD: (u64, u64) = (0x800_0000, 0x1_0000);

fn gicr_regions(n: u64) -> Vec<(u64, u64)> {
    (0..n).map(|i| (0x1_0000_0000 + i * 0x100_0000, 0x40_0000)).collect()
}

#[test]
fn every_redistributor_region_of_the_device_tree_is_kept() {
    // Five regions (six reg entries), then GICC, GICH and GICV.
    let regions = gicr_regions(5);
    let tail = [(0x2c00_0000, 0x2_0000), (0x2c01_0000, 0x2_0000), (0x2c02_0000, 0x2_0000)];
    for with_tail in [false, true] {
        let mut regs = vec![GICD];
        regs.extend_from_slice(&regions);
        if with_tail {
            regs.extend_from_slice(&tail);
        }
        let h = dt_hw("arm,gic-v3", &regs, Some(5));
        assert_eq!(h.gic_version, 3);
        assert_eq!(h.gicd, GICD.0);
        assert!(!h.gicr_lost, "tail {with_tail}");
        assert_eq!(&h.gicr[..h.n_gicr], &regions[..], "tail {with_tail}");
    }
}

#[test]
fn more_redistributor_regions_than_recorded_are_a_boot_failure() {
    let n = hw::MAX_GICR as u64 + 1;
    let mut regs = vec![GICD];
    regs.extend(gicr_regions(n));
    let h = dt_hw("arm,gic-v3", &regs, Some(n as u32));
    assert!(h.gicr_lost);
    // So is a count the reg property does not hold, or none at all.
    let mut regs = vec![GICD];
    regs.extend(gicr_regions(2));
    assert!(dt_hw("arm,gic-v3", &regs, Some(3)).gicr_lost);
    assert!(dt_hw("arm,gic-v3", &regs, Some(0)).gicr_lost);
    // Exactly as many as recorded is fine.
    let mut regs = vec![GICD];
    regs.extend(gicr_regions(hw::MAX_GICR as u64));
    let h = dt_hw("arm,gic-v3", &regs, Some(hw::MAX_GICR as u32));
    assert!(!h.gicr_lost);
    assert_eq!(h.n_gicr, hw::MAX_GICR);
}

#[test]
fn gicc_gich_and_gicv_are_not_redistributor_regions() {
    // One region (the default, or said), then 128 KiB GICC, GICH and
    // GICV (64 KiB-spaced aliases): none of them is a redistributor.
    let regs = [GICD, (0x80a_0000, 0xf6_0000), (0x2c00_0000, 0x2_0000), (0x2c01_0000, 0x2_0000), (0x2c02_0000, 0x2_0000)];
    for redist in [None, Some(1)] {
        let h = dt_hw("arm,gic-v3", &regs, redist);
        assert!(!h.gicr_lost);
        assert_eq!(&h.gicr[..h.n_gicr], &[(0x80a_0000, 0xf6_0000)], "{redist:?}");
    }
}

#[test]
fn a_gicv2_cpu_interface_is_kept_whole() {
    let h = dt_hw("arm,gic-400", &[(0x2c00_1000, 0x1000), (0x2c02_0000, 0x2_0000)], None);
    assert_eq!(h.gic_version, 2);
    assert_eq!((h.gicc, h.gicc_len), (0x2c02_0000, 0x2_0000));
}

#[test]
fn acpi_and_dt_parse_virt_fields() {
    // ACPI: MADT GICC with GICV (off 40), GICH (off 48), VGIC maintenance
    // (off 56) + GTDT with vtimer (off 64) and hyptimer (off 72).
    let mut gicc_e = gicc(0, 0);
    gicc_e[32..40].copy_from_slice(&0x801_0000u64.to_le_bytes()); // GICC
    gicc_e[40..48].copy_from_slice(&0x804_0000u64.to_le_bytes()); // GICV
    gicc_e[48..56].copy_from_slice(&0x803_0000u64.to_le_bytes()); // GICH
    gicc_e[56..60].copy_from_slice(&25u32.to_le_bytes()); // VGIC maint
    let mut gicd_e = gicd();
    gicd_e[20] = 2; // GICv2
    let madt_entries = madt(&[gicd_e, gicc_e]);
    let madt_len = 44 + madt_entries.len();
    let gtdt_len = 80usize;
    let mut mem = vec![0u8; BASE as usize + 512];
    let buf = &mut mem[BASE as usize..];
    // RSDP at 0, XSDT at 64 (2 entries = 52 bytes), MADT at 128, GTDT at 320.
    let madt_off = 128usize;
    let gtdt_off = 320usize;
    buf[madt_off..madt_off + 4].copy_from_slice(b"APIC");
    buf[madt_off + 4..madt_off + 8].copy_from_slice(&(madt_len as u32).to_le_bytes());
    buf[madt_off + 44..madt_off + madt_len].copy_from_slice(&madt_entries);
    checksum(&mut buf[madt_off..madt_off + madt_len], 9);
    buf[gtdt_off..gtdt_off + 4].copy_from_slice(b"GTDT");
    buf[gtdt_off + 4..gtdt_off + 8].copy_from_slice(&(gtdt_len as u32).to_le_bytes());
    buf[gtdt_off + 48..gtdt_off + 52].copy_from_slice(&30u32.to_le_bytes());
    buf[gtdt_off + 64..gtdt_off + 68].copy_from_slice(&27u32.to_le_bytes());
    buf[gtdt_off + 72..gtdt_off + 76].copy_from_slice(&26u32.to_le_bytes());
    checksum(&mut buf[gtdt_off..gtdt_off + gtdt_len], 9);
    let xsdt_len = 36 + 16;
    buf[64..68].copy_from_slice(b"XSDT");
    buf[68..72].copy_from_slice(&(xsdt_len as u32).to_le_bytes());
    buf[64 + 36..64 + 44].copy_from_slice(&(BASE + madt_off as u64).to_le_bytes());
    buf[64 + 44..64 + 52].copy_from_slice(&(BASE + gtdt_off as u64).to_le_bytes());
    checksum(&mut buf[64..64 + xsdt_len], 9);
    buf[..8].copy_from_slice(b"RSD PTR ");
    buf[15] = 2;
    buf[20..24].copy_from_slice(&36u32.to_le_bytes());
    buf[24..32].copy_from_slice(&(BASE + 64).to_le_bytes());
    checksum(&mut buf[..20], 8);
    checksum(&mut buf[..36], 32);
    layout::MEMORY.set((mem.as_ptr() as u64, mem.len() as u64));
    let h = hw::discover(BASE, 0).expect("ACPI virt");
    layout::MEMORY.set((0, 0));
    assert_eq!(h.gic_version, 2);
    assert_eq!(h.gich, 0x803_0000);
    assert_eq!(h.gicv, 0x804_0000);
    assert_eq!(h.vgic_maint, 25);
    assert_eq!(h.ptimer, 30);
    assert_eq!(h.vtimer, 27);
    assert_eq!(h.hyptimer, 26);

    // DT: GICv2 with GICD, GICC, GICH, GICV + maintenance PPI 25, and timer
    // node with vtimer PPI 27 and hyptimer PPI 26.
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("intc@8000000");
    d.prop_str("compatible", "arm,cortex-a15-gic");
    d.prop("reg", &reg2(&[
        (0x800_0000, 0x1_0000),
        (0x801_0000, 0x1_0000),
        (0x803_0000, 0x1_0000),
        (0x804_0000, 0x1_0000),
    ]));
    // PPI 9 -> 16 + 9 = 25
    let maint: Vec<u8> = [1u32, 9, 4].iter().flat_map(|w| w.to_be_bytes()).collect();
    d.prop("interrupts", &maint);
    d.end();
    d.begin("timer");
    // sec=13, nonsec=14, virt=11 (27), hyp=10 (26)
    let timers: Vec<u8> = [1u32, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4]
        .iter()
        .flat_map(|w| w.to_be_bytes())
        .collect();
    d.prop("interrupts", &timers);
    d.end();
    d.end();
    let blob = d.blob();
    let mut dt_mem = vec![0u8; BASE as usize + blob.len()];
    dt_mem[BASE as usize..].copy_from_slice(&blob);
    layout::MEMORY.set((dt_mem.as_ptr() as u64, dt_mem.len() as u64));
    let h = hw::discover(0, BASE).expect("DT virt");
    layout::MEMORY.set((0, 0));
    assert_eq!(h.gic_version, 2);
    assert_eq!((h.gich, h.gich_len), (0x803_0000, 0x1_0000));
    assert_eq!((h.gicv, h.gicv_len), (0x804_0000, 0x1_0000));
    assert_eq!(h.vgic_maint, 25);
    assert_eq!(h.ptimer, 30);
    assert_eq!(h.vtimer, 27);
    assert_eq!(h.hyptimer, 26);
}

#[test]
fn kernel_device_ranges_are_kept_in_whole_pages() {
    assert_eq!(bootmem::pages_covering(0x800_0000, 0x801_0000), (0x800_0000, 0x801_0000));
    assert_eq!(bootmem::pages_covering(0x800_0800, 0x801_0800), (0x800_0000, 0x801_1000));
    assert_eq!(bootmem::pages_covering(0x800_0fff, 0x800_1001), (0x800_0000, 0x800_2000));
    assert_eq!(bootmem::pages_covering(u64::MAX - 0x10, u64::MAX), (u64::MAX & !0xfff, u64::MAX & !0xfff));
    // Through the device list: no page of an unaligned GIC is left over.
    let mut dev = bootmem::Ranges::new();
    dev.add(0, 0x1000_0000);
    let (s, e) = bootmem::pages_covering(0x800_0800, 0x800_0800 + 0x1_0000);
    dev.remove(s, e);
    assert_eq!(dev.n, 2);
    assert_eq!((dev.r[0].end, dev.r[1].start), (0x800_0000, 0x801_1000));
}

#[test]
fn an_unbounded_redistributor_range_must_end_inside_the_mapping() {
    let b = 0x800_0000;
    // No bound from the tables, mapped for two cores: two redistributors
    // and the last frame, or more redistributors past the mapping.
    let closed = [(b, 0), (b + 0x2_0000, LAST)];
    assert_eq!(gic_layout::reserve(b, 0, 0x8_0000, frames(&closed)), Some(0x8_0000));
    let open = [(b, 0), (b + 0x2_0000, 0), (b + 0x4_0000, 0), (b + 0x6_0000, 0)];
    assert_eq!(gic_layout::reserve(b, 0, 0x8_0000, frames(&open)), None);
    // GICv4: the last frame's VLPI frames may pass the mapping.
    let v4 = [(b, VLPIS), (b + 0x4_0000, VLPIS | LAST)];
    assert_eq!(gic_layout::reserve(b, 0, 0x6_0000, frames(&v4)), Some(0x8_0000));
    // A bound from the tables: all of it, last frame seen or not.
    assert_eq!(gic_layout::reserve(b, 0x8_0000, 0x8_0000, frames(&open)), Some(0x8_0000));
    assert_eq!(gic_layout::reserve(b, 0x2_0000, 0x2_0000, frames(&[(b, VLPIS)])), Some(0x4_0000));
}

/// The hardware a device tree describes whose root has `root` cells and
/// whose GICv3 sits under /soc with `soc` cells and a raw `reg`.
fn dt_soc(root: (u32, u32), soc: (u32, u32), reg: &[u8], redist: u32) -> Option<hw::Hw> {
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", root.0);
    d.prop_u32("#size-cells", root.1);
    d.begin("cpus");
    d.prop_u32("#address-cells", 1);
    d.prop_u32("#size-cells", 0);
    d.begin("cpu@0");
    d.prop("reg", &0u32.to_be_bytes());
    d.end();
    d.end();
    d.begin("soc");
    d.prop_u32("#address-cells", soc.0);
    d.prop_u32("#size-cells", soc.1);
    // The root's cells for /soc's own reg.
    d.prop("reg", &[0u8; 12]);
    d.prop("ranges", &[]);
    d.begin("interrupt-controller@8000000");
    d.prop_str("compatible", "arm,gic-v3");
    // The GIC's cells are its children's (an ITS), not its own reg's.
    d.prop_u32("#address-cells", 1);
    d.prop_u32("#size-cells", 1);
    d.prop("reg", reg);
    d.prop_u32("#redistributor-regions", redist);
    d.end();
    d.end();
    d.end();
    let blob = d.blob();
    let mut mem = vec![0u8; BASE as usize + blob.len()];
    mem[BASE as usize..].copy_from_slice(&blob);
    layout::MEMORY.set((mem.as_ptr() as u64, mem.len() as u64));
    let h = hw::discover(0, BASE);
    layout::MEMORY.set((0, 0));
    h
}

/// Proposal 5: reg is read in its parent's cells, not the root's.
#[test]
fn reg_uses_the_parents_cells() {
    let regions = gicr_regions(3);
    let mut regs = vec![GICD];
    regs.extend_from_slice(&regions);
    let h = dt_soc((2, 1), (2, 2), &reg2(&regs), 3).expect("device tree");
    assert_eq!(h.gicd, GICD.0);
    assert!(!h.gicr_lost);
    assert_eq!(&h.gicr[..h.n_gicr], &regions[..]);
    assert_eq!(&h.cpus[..h.n_cpus], &[0]);
    // One address cell and one size cell under /soc.
    let one: Vec<u8> = [(0x800_0000u32, 0x1_0000u32), (0x80a_0000, 0xf6_0000)]
        .iter()
        .flat_map(|&(a, s)| [a.to_be_bytes(), s.to_be_bytes()].concat())
        .collect();
    let h = dt_soc((2, 2), (1, 1), &one, 1).expect("device tree");
    assert_eq!(h.gicd, 0x800_0000);
    assert!(!h.gicr_lost);
    assert_eq!(&h.gicr[..h.n_gicr], &[(0x80a_0000, 0xf6_0000)]);
}

#[test]
fn a_gic_reg_the_kernel_cannot_read_whole_is_never_used() {
    let mut regs = vec![GICD];
    regs.extend(gicr_regions(2));
    // Cells the kernel does not handle: no GIC found, so no boot.
    assert!(dt_soc((2, 2), (3, 2), &reg2(&regs), 2).is_none());
    assert!(dt_soc((2, 2), (2, 0), &reg2(&regs), 2).is_none());
    // A reg that is not whole entries is not read at all: no GIC either.
    let mut bytes = reg2(&regs);
    bytes.extend_from_slice(&[0; 8]);
    assert!(dt_soc((2, 2), (2, 2), &bytes, 2).is_none());
    assert!(dt_soc((2, 2), (2, 2), &reg2(&regs), 2).is_some());
}

/// The AArch64 loader sizes its memory map buffer with `capacity`: the
/// sizing call's map and 16 more descriptors of at least 48 bytes, None
/// where that does not fit (as x86_mmap::capacity).
#[test]
fn the_efi_memory_map_capacity_never_wraps() {
    assert_eq!(efi_mmap::capacity(4800, 48), Some(4800 + 16 * 48));
    assert_eq!(efi_mmap::capacity(4800, 0), Some(4800 + 16 * 48));
    assert_eq!(efi_mmap::capacity(4800, 80), Some(4800 + 16 * 80));
    assert_eq!(efi_mmap::capacity(usize::MAX - 16 * 48, 48), Some(usize::MAX));
    assert_eq!(efi_mmap::capacity(usize::MAX - 16 * 48 + 1, 48), None);
    assert_eq!(efi_mmap::capacity(0, usize::MAX / 16 + 1), None);
    let lib = include_str!("../../crates/fl4-efi/src/lib.rs");
    assert!(lib.contains("let cap = efi_mmap::capacity(size, dsize).unwrap_or_else(|| fail(\"GetMemoryMap\"));"));
}

/// UEFI 2.10 §7.4: after the first ExitBootServices call, even a failed
/// one, firmware may have shut boot services down in part, and the loader
/// may call only GetMemoryMap and ExitBootServices: every failure in the
/// retry loop halts through `handoff_failed`, which uses the console
/// (`fail`) only before that first call.
#[test]
fn the_aarch64_loader_uses_no_console_after_exit_boot_services() {
    let lib = include_str!("../../crates/fl4-efi/src/lib.rs");
    let at = lib.find("    let mut tries = 0;\n    loop {\n").expect("handoff loop");
    let end = at + lib[at..].find("\n    }\n").expect("loop end");
    let body = &lib[at..end];
    assert!(body.contains("(bs().exit_boot_services)(image, key)"));
    assert!(!body.contains(" fail("), "a failure after ExitBootServices writes to the console:\n{body}");
    assert_eq!(body.matches("handoff_failed(tries, ").count(), 3);
    let h = &lib[lib.find("fn handoff_failed(tries: usize, msg: &str) -> ! {").expect("handoff_failed")..];
    let h = &h[..h.find("\n}\n").unwrap()];
    assert!(h.contains("if tries == 0 {\n        fail(msg);\n    }"), "{h}");
    assert!(h.contains("msr daifset, #0xf") && h.contains("wfi"), "{h}");
    assert_eq!(h.matches("fail(").count(), 1);
}

// ---- extra BootInfo ---------------------------------------------------------------

/// The records of an extra BootInfo of `len` bytes, as user space walks
/// them: (id, offset, length); every header inside, none of length 0.
fn bi_records(buf: &[u8], len: usize) -> Vec<(u64, usize, usize)> {
    let mut out = vec![];
    let mut off = 0;
    while off < len {
        assert!(off + 16 <= len, "header at {off:#x} past the end {len:#x}");
        let id = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
        let l = u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap()) as usize;
        assert!(l >= 16 && off + l <= len, "record {id} at {off:#x} of {l:#x} bytes, end {len:#x}");
        out.push((id, off, l));
        off += l;
    }
    out
}

/// Bytes without 0xAA, the poison of the buffers below.
fn not_poison(i: usize, salt: u8) -> u8 {
    match (i as u8).wrapping_mul(31).wrapping_add(salt) {
        0xaa => 0x55,
        b => b,
    }
}

/// Audit r10 F2: the extra BootInfo is a chain of records each exactly as
/// long as its header says, that ends exactly at the length given to user
/// space, a multiple of 8, and has no byte the kernel did not write: the
/// buffer is free RAM the firmware left, poisoned here. Before, with the
/// ACPI RSDP (52 bytes) the device tree record started 4 bytes off 8, and
/// rounding the end up added 4 bytes past the record (a device tree of a
/// multiple of 8) or none to it (4 more), so the chain ran past or stopped
/// short of the end; the padding and the tail came from the old RAM.
#[test]
fn extra_bootinfo_records_chain_exactly_with_nothing_uninitialized() {
    use fl4_abi::consts::bootinfo_header as id;
    let rsdp: Vec<u8> = (0..36).map(|i| not_poison(i, 1)).collect();
    let regs = [0x1111_2222_3333_4444, 0x0123_4567_89ab_cdef, 0, u64::MAX >> 1, 0x10];
    let all_mods: Vec<fl4_bootinfo::Module> = (0..fl4_bootinfo::MAX_MODULES)
        .map(|m| {
            let mut name = [0u8; fl4_bootinfo::NAME_LEN];
            for (i, c) in name.iter_mut().enumerate().take(10) {
                *c = not_poison(i, m as u8);
            }
            fl4_bootinfo::Module { paddr: 0x4000_0000 + m as u64 * 0x10_0000, size: 0x1234 + m as u64, name }
        })
        .collect();
    let tpm = extra_bi::TpmEventLog { paddr: 0x4800_0000, size: 0x1_2345, flags: 1 };
    let msi_words = [0x808_0040u64, 256, 64, 4096];
    for has_msi in [false, true] {
    for has_tpm in [false, true] {
        for has_rsdp in [false, true] {
            for n_mods in [0, 1, 3, fl4_bootinfo::MAX_MODULES] {
                for dtb_len in [0usize, 40, 41, 44, 45, 48, 0x1000, 0x1003, 0x1004, 0x1_0000] {
                    let what = format!("msi {has_msi} tpm {has_tpm} rsdp {has_rsdp} mods {n_mods} dtb {dtb_len:#x}");
                    let mut dtb: Vec<u8> = (0..dtb_len).map(|i| not_poison(i, 7)).collect();
                    if dtb_len >= 8 {
                        dtb[..4].copy_from_slice(&0xd00d_feedu32.to_be_bytes());
                        dtb[4..8].copy_from_slice(&0x0002_0000u32.to_be_bytes());
                    }
                    // The kernel's size for it (lib.rs `extra_size`).
                    let size = 4096 + ((dtb_len + 16 + 4096) & !4095);
                    let mut buf = vec![0xaau8; size];
                    let mods = &all_mods[..n_mods];
                    let vi = extra_bi::VirtInfo {
                        flags: 0x7,
                        ipa_bits: 40,
                        gic_version: 3,
                        vgic_vtr: 0x9000_0003,
                        gicv_paddr: 0x804_0000,
                        gicv_size: 0x1_0000,
                        midr_el1: 0x410f_d083,
                        ctr_el0: 0x8444_c004,
                        id_aa64mmfr0_el1: 0x10_1122,
                        id_aa64mmfr1_el1: 0x220,
                        id_aa64mmfr2_el1: 0x10,
                        id_aa64dfr0_el1: 0x1030_5106,
                        id_aa64dfr1_el1: 0,
                    };
                    let len = extra_bi::build(&mut buf, has_rsdp.then_some(&rsdp[..]), &regs, Some(&vi), mods, has_tpm.then_some(&tpm), has_msi.then_some(&msi_words), &dtb).expect(&what);
                    assert_eq!(len % 8, 0, "{what}");
                    assert!(buf.iter().all(|&b| b != 0xaa), "{what}: uninitialized bytes");
                    let recs = bi_records(&buf, len);
                    let mut want = vec![];
                    if has_rsdp {
                        want.push(id::X86_ACPI_RSDP);
                    }
                    want.push(id::AARCH64_ID_REGS);
                    want.push(id::AARCH64_VIRT_INFO);
                    if n_mods != 0 {
                        want.push(id::X86_BOOT_MODULES);
                    }
                    if has_tpm {
                        want.push(id::TPM_EVENT_LOG);
                    }
                    if has_msi {
                        want.push(id::AARCH64_MSI_INFO);
                    }
                    if dtb_len != 0 {
                        want.push(id::FDT);
                    }
                    if recs.last().unwrap().0 == id::PADDING {
                        want.push(id::PADDING);
                    }
                    assert_eq!(recs.iter().map(|r| r.0).collect::<Vec<_>>(), want, "{what}");
                    for &(rid, at, l) in &recs {
                        let body = &buf[at + 16..at + l];
                        match rid {
                            id::X86_ACPI_RSDP => assert_eq!(body, &rsdp[..], "{what}"),
                            id::AARCH64_ID_REGS => {
                                let got: Vec<u64> = body.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
                                assert_eq!(got, regs, "{what}");
                            }
                            id::AARCH64_VIRT_INFO => {
                                assert_eq!(l, 16 + 13 * 8, "{what}");
                                let got: Vec<u64> = body.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
                                assert_eq!(got, vi.to_words(), "{what}");
                            }
                            id::X86_BOOT_MODULES => {
                                assert_eq!(l, 16 + 8 + 80 * n_mods, "{what}");
                                assert_eq!(u64::from_le_bytes(body[..8].try_into().unwrap()), n_mods as u64);
                                for (m, e) in mods.iter().zip(body[8..].chunks(80)) {
                                    assert_eq!(u64::from_le_bytes(e[..8].try_into().unwrap()), m.paddr);
                                    assert_eq!(u64::from_le_bytes(e[8..16].try_into().unwrap()), m.size);
                                    assert_eq!(&e[16..], &m.name[..]);
                                }
                            }
                            id::FDT => {
                                assert_eq!(l, 16 + dtb_len.div_ceil(8) * 8, "{what}");
                                let total = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                                assert_eq!(total, dtb_len, "{what}: FDT totalsize");
                                assert_eq!(&body[..4], &dtb[..4]);
                                assert_eq!(&body[8..dtb_len], &dtb[8..], "{what}");
                                assert!(body[dtb_len..].iter().all(|&b| b == 0), "{what}");
                            }
                            id::TPM_EVENT_LOG => {
                                assert_eq!(l, 40, "{what}");
                                let got: Vec<u64> = body.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
                                assert_eq!(got, [tpm.paddr, tpm.size, tpm.flags], "{what}");
                            }
                            id::AARCH64_MSI_INFO => {
                                assert_eq!(l, 48, "{what}");
                                let got: Vec<u64> = body.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
                                assert_eq!(got, msi_words, "{what}");
                            }
                            id::PADDING => {
                                assert!(l < 24, "{what}");
                                assert!(body.iter().all(|&b| b == 0), "{what}");
                            }
                            _ => panic!("{what}: record {rid}"),
                        }
                    }
                }
            }
        }
    }
    }
    // Too small a buffer is refused, not overrun.
    let mut small = [0u8; 64];
    assert_eq!(extra_bi::build(&mut small, Some(&rsdp), &regs, None, &[], None, None, &[]), None);
    let mut small = vec![0u8; 4096];
    assert_eq!(extra_bi::build(&mut small, None, &regs, None, &[], None, None, &[1u8; 4096]), None);
}

/// Audit r13 T3-01: the EL2 nVHE identity map (TTBR0_EL2) keeps the W^X
/// split of TTBR1_EL1. Before, the whole kernel region was mapped
/// read-write and executable at EL2 (`normal() | D_AP_RW_EL2`, no XN).
/// EL2 regime, E2H=0: AP[2:1] = 0b01 RW, 0b11 RO; bit 54 (`D_UXN`) is XN.
#[test]
fn the_el2_identity_map_is_w_xor_x() {
    let paging = include_str!("../../crates/fl4-kernel-aarch64/src/paging.rs");
    assert!(paging.contains("const D_UXN: u64 = 1 << 54;"));
    let hyp = &paging[paging.find("if cpu::is_el2_nvhe() {").expect("EL2 nVHE map")..];
    let hyp = &hyp[..hyp.find("s.hyp_ident = hyp;").expect("end of the EL2 map")];
    assert!(hyp.contains("const D_AP_RW_EL2: u64 = 1 << 6;"));
    assert!(hyp.contains("const D_AP_RO_EL2: u64 = 3 << 6;"));
    let maps: Vec<&str> = hyp.lines().map(str::trim).filter(|l| l.starts_with("map(hyp, r_start")).collect();
    assert_eq!(
        maps,
        [
            "map(hyp, r_start, r_start, text_end, normal() | D_AP_RO_EL2);",
            "map(hyp, r_start + text_end, r_start + text_end, ro_end - text_end, normal() | D_AP_RO_EL2 | D_UXN);",
            "map(hyp, r_start + ro_end, r_start + ro_end, region_len - ro_end, normal() | D_AP_RW_EL2 | D_UXN);",
        ],
        "text RO+X, rodata RO+XN, the rest RW+XN"
    );
    // Nothing else in the EL2 map is both writable and executable.
    for l in hyp.lines().map(str::trim).filter(|l| l.starts_with("map(hyp") || l.starts_with("let ram_attr")) {
        assert!(l.contains("D_UXN") || !l.contains("D_AP_RW_EL2"), "RW without XN at EL2: {l}");
    }
    // The split points are the TTBR1 ones.
    assert!(paging.contains("let text_end = unsafe { &__text_end as *const u8 as u64 } - KERNEL_VBASE;"));
    assert!(paging.contains("let ro_end = unsafe { &__rodata_end as *const u8 as u64 } - KERNEL_VBASE;"));
}

/// Audit r13 T3-02: every `HypCpu` offset the Rust side asserts is the one
/// hyp_nvhe.S uses, field by field (comparing only the set of values would
/// miss two swapped names).
#[test]
fn hyp_cpu_offsets_match_the_assembly_field_by_field() {
    const FIELDS: [(&str, &str); 42] = [
        ("in_guest", "HYP_IN_GUEST"),
        ("regs_pa", "HYP_REGS_PA"),
        ("vcpu_pa", "HYP_VCPU_PA"),
        ("host_hcr_el2", "HYP_HOST_HCR"),
        ("host_mdcr_el2", "HYP_HOST_MDCR"),
        ("guest_mdcr_el2", "HYP_GUEST_MDCR"),
        ("gic_v3", "HYP_GIC_V3"),
        ("vgic_lrs", "HYP_VGIC_LRS"),
        ("vgic_n_apr", "HYP_VGIC_N_APR"),
        ("has_ras", "HYP_HAS_RAS"),
        ("host_elr_el2", "HYP_HOST_ELR"),
        ("host_spsr_el2", "HYP_HOST_SPSR"),
        ("host_ttbr0_el1", "HYP_HOST_TTBR0"),
        ("host_cpacr_el1", "HYP_HOST_CPACR"),
        ("host_sp_el1", "HYP_HOST_SP_EL1"),
        ("host_sctlr_el1", "HYP_HOST_SCTLR"),
        ("host_tcr_el1", "HYP_HOST_TCR"),
        ("host_ttbr1_el1", "HYP_HOST_TTBR1"),
        ("host_mair_el1", "HYP_HOST_MAIR"),
        ("host_vbar_el1", "HYP_HOST_VBAR"),
        ("host_tpidr_el1", "HYP_HOST_TPIDR_EL1"),
        ("host_cntkctl_el1", "HYP_HOST_CNTKCTL"),
        ("host_mdscr_el1", "HYP_HOST_MDSCR"),
        ("guest_mdccint_nz", "HYP_GUEST_MDCCINT_NZ"),
        ("guest_ras_nz", "HYP_GUEST_RAS_NZ"),
        ("guest_cntvoff_nz", "HYP_GUEST_CNTVOFF_NZ"),
        ("cur_actlr", "HYP_CUR_ACTLR"),
        ("cur_vttbr", "HYP_CUR_VTTBR"),
        ("cur_vpidr", "HYP_CUR_VPIDR"),
        ("cur_vmpidr", "HYP_CUR_VMPIDR"),
        ("vgic_live", "HYP_VGIC_LIVE"),
        ("vgic_lr_mask", "HYP_VGIC_LR_MASK"),
        ("cur_vmcr", "HYP_CUR_VMCR"),
        ("cur_ap0r", "HYP_CUR_AP0R"),
        ("cur_ap1r", "HYP_CUR_AP1R"),
        ("host_actlr_el1", "HYP_HOST_ACTLR"),
        ("cur_cptr", "HYP_CUR_CPTR"),
        ("cur_cntv_ctl", "HYP_CUR_CNTV_CTL"),
        ("cur_cntv_cval", "HYP_CUR_CNTV_CVAL"),
        ("cur_csselr", "HYP_CUR_CSSELR"),
        ("cur_mdscr", "HYP_CUR_MDSCR"),
        ("vgic_owner", "HYP_VGIC_OWNER"),
    ];
    fn hex(v: &str) -> u64 {
        u64::from_str_radix(v.trim().trim_start_matches("0x"), 16).unwrap_or_else(|_| panic!("offset {v}"))
    }
    let rs = include_str!("../../crates/fl4-arch-aarch64/src/hyp_nvhe.rs");
    let asm = include_str!("../../crates/fl4-arch-aarch64/src/hyp_nvhe.S");
    let mut rust = std::collections::BTreeMap::new();
    for l in rs.lines() {
        if let Some(rest) = l.trim().strip_prefix("const _: () = assert!(offset_of!(HypCpu, ") {
            let (field, rest) = rest.split_once(") == ").expect("offset assertion");
            let v = rest.strip_suffix(");").expect("offset assertion end");
            assert!(rust.insert(field.to_string(), hex(v)).is_none(), "{field} twice");
        }
    }
    let mut equ = std::collections::BTreeMap::new();
    for l in asm.lines() {
        if let Some(rest) = l.trim().strip_prefix(".equ HYP_") {
            let (name, v) = rest.split_once(',').expect(".equ");
            let name = format!("HYP_{}", name.trim());
            if name.starts_with("HYP_KIND_") || name == "HYP_SPSR_EL1H_MASKED" {
                continue;
            }
            assert!(equ.insert(name.clone(), hex(v)).is_none(), "{name} twice");
        }
    }
    assert_eq!(rust.len(), FIELDS.len(), "every offset_of! assertion is mapped");
    assert_eq!(equ.len(), FIELDS.len(), "every HypCpu .equ is mapped");
    for (field, name) in FIELDS {
        let r = rust.get(field).unwrap_or_else(|| panic!("no offset_of!(HypCpu, {field})"));
        let a = equ.get(name).unwrap_or_else(|| panic!("no .equ {name}"));
        assert_eq!(r, a, "{field} is at {r:#x}, {name} says {a:#x}");
    }
    // boot.S records the host HCR_EL2/MDCR_EL2 pair by raw offset.
    let boot = include_str!("../../crates/fl4-kernel-aarch64/src/boot.S");
    assert!(boot.contains("stp     x9, x10, [sp, #0x18]    /* HypCpu.host_hcr_el2, host_mdcr_el2 */"));
    assert_eq!((rust["host_hcr_el2"], rust["host_mdcr_el2"]), (0x18, 0x20));
}

/// Audit r13 T2-02: the free RAM ranges dropped to fit BootParams (the
/// smallest first) and device ranges past `MAX_REGIONS` are reported, not
/// lost silently.
#[test]
fn ranges_past_the_boot_range_limit_are_reported() {
    let lib = include_str!("../../crates/fl4-kernel-aarch64/src/lib.rs");
    let drop = &lib[lib.find("while free.n > MAX_REGIONS {").expect("smallest dropped")..];
    let drop = &drop[..drop.find("\n    }\n").unwrap()];
    assert!(drop.contains("free.remove(r.start, r.end);"));
    assert!(drop.contains("omitted_free += 1;") && drop.contains("omitted_bytes += r.end - r.start;"));
    assert!(lib.contains("kprintln!(\"FL4: {} free RAM region(s) ({:#x} bytes) exceed the boot range limit\", omitted_free, omitted_bytes);"));
    let dev = lib.find("for i in 0..dev.n.min(MAX_REGIONS) {").expect("device ranges copied");
    let warn = lib.find("if dev.n > MAX_REGIONS {").expect("device ranges past the limit");
    assert!(dev < warn);
    assert!(lib.contains("kprintln!(\"FL4: {} device memory regions exceed the boot range limit\", dev.n - MAX_REGIONS);"));
}

// ---- pre-zeroed RAM & /cpus/cpu ---------------------------------------------------

#[test]
fn zeroed_bootmem_ranges_carve_out_dtb_and_convert_to_boot_params() {
    let mut zeroed = bootmem::Ranges::new();
    zeroed.add(0x4000_0000, 0x8_4000_0000);
    zeroed.remove(0x4001_0000, 0x4001_1000);
    assert!(!zeroed.lost);
    assert_eq!(zeroed.n, 2);
    assert_eq!(zeroed.r[0], boot::Region { start: 0x4000_0000, end: 0x4001_0000 });
    assert_eq!(zeroed.r[1], boot::Region { start: 0x4001_1000, end: 0x8_4000_0000 });
}

#[test]
fn single_core_dt_cpu_without_unit_address_is_registered() {
    let mut d = Dtb::default();
    d.begin("");
    d.prop_u32("#address-cells", 2);
    d.prop_u32("#size-cells", 2);
    d.begin("cpus");
    d.prop_u32("#address-cells", 1);
    d.prop_u32("#size-cells", 0);
    d.begin("cpu");
    d.prop("reg", &0u32.to_be_bytes());
    d.end();
    d.end();
    d.begin("intc@8000000");
    d.prop_str("compatible", "arm,gic-v3");
    d.prop("reg", &reg2(&[GICD, (0x80a_0000, 0xf6_0000)]));
    d.end();
    d.end();
    let blob = d.blob();
    let mut mem = vec![0u8; BASE as usize + blob.len()];
    mem[BASE as usize..].copy_from_slice(&blob);
    layout::MEMORY.set((mem.as_ptr() as u64, mem.len() as u64));
    let h = hw::discover(0, BASE).expect("device tree");
    layout::MEMORY.set((0, 0));
    assert_eq!(h.n_cpus, 1);
    assert_eq!(h.cpus[0], 0);
}
