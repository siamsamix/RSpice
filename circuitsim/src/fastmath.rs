//! Batch transcendental math with an AVX2 backend and a scalar fallback
//! (TODO §3 "AVX2/SIMD-optimized transcendental math").
//!
//! LTspice ships `exp_avx2.c`, `log_avx2.c`, `sin_avx2.c`, … for its device and
//! behavioral-source equations. This module is RSpice's counterpart: slice
//! kernels `exp`, `ln`, `log10`, `sin`, `cos`, `tan` and `pow` that evaluate
//! four `f64` lanes at a time with AVX2 + FMA when the CPU has them (runtime
//! detection, so a default `cargo build` still benefits) and fall back to the
//! standard library's scalar functions otherwise.
//!
//! # Contract
//!
//! * **Scalar backend ≡ libm.** [`Backend::Scalar`] calls `f64::exp` & co.
//!   one element at a time, so with it every result is bit-identical to what
//!   the engine computes today.
//! * **AVX2 backend ≤ 4 ulp** (`pow`: ≤ `4 + 2·|y·ln x|` ulp — the product's
//!   rounding is amplified by the exponential) against the correctly rounded
//!   value on the normal domain — *not* bit-identical to libm. That is why the
//!   engine only reaches it through the opt-in `.options fastmath=1`; the
//!   default keeps every deck's results byte-stable (see `docs/adr/0005`).
//! * **Position independent.** A value's result never depends on its index or
//!   on the batch length: the tail of a slice runs through the same vector
//!   kernel, padded with a benign lane.
//! * Specials follow IEEE/libm: `exp(±inf)`, `ln(0) = −inf`, `ln(x<0) = NaN`,
//!   NaN in → NaN out, overflow → `inf`, underflow through the subnormals.
//!   `sin`/`cos`/`tan` beyond |x| = 1e5 (and non-finite inputs) are computed
//!   by libm per lane, where a 3-part Cody–Waite reduction would lose bits.
//! * `log10` and `pow` are not correctly rounded at exact decades / integer
//!   powers; use the scalar functions when exactness there matters.

use std::sync::atomic::{AtomicU8, Ordering};

/// Which kernel family a call runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// 4-lane AVX2 + FMA kernels (x86-64 CPUs with both features).
    Avx2,
    /// The standard library's scalar functions, one element at a time.
    Scalar,
}

static DETECTED: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 scalar, 2 avx2

/// The backend [`exp`] & co. dispatch to: AVX2 when the CPU has AVX2 and FMA,
/// unless `RSPICE_NO_SIMD` is set (any value) — the switch for A/B timing and
/// for ruling SIMD in or out while chasing a numerical difference.
pub fn backend() -> Backend {
    match DETECTED.load(Ordering::Relaxed) {
        1 => Backend::Scalar,
        2 => Backend::Avx2,
        _ => {
            let chosen = detect();
            DETECTED.store(if chosen == Backend::Avx2 { 2 } else { 1 }, Ordering::Relaxed);
            chosen
        }
    }
}

fn detect() -> Backend {
    if std::env::var_os("RSPICE_NO_SIMD").is_some() {
        return Backend::Scalar;
    }
    avx2_available()
}

/// Does this CPU (and build target) have what the AVX2 kernels need?
pub fn avx2_available() -> Backend {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            return Backend::Avx2;
        }
    }
    Backend::Scalar
}

macro_rules! unary {
    ($(#[$doc:meta])* $name:ident, $with:ident, $scalar:expr, $kernel:ident, $fix:expr) => {
        $(#[$doc])*
        pub fn $name(x: &[f64], out: &mut [f64]) {
            $with(backend(), x, out)
        }

        /// As the unqualified function, on an explicit [`Backend`] (an
        /// unavailable AVX2 request degrades to the scalar path).
        pub fn $with(backend: Backend, x: &[f64], out: &mut [f64]) {
            assert_eq!(x.len(), out.len(), "fastmath: input/output length mismatch");
            #[cfg(target_arch = "x86_64")]
            {
                if backend == Backend::Avx2 && avx2_available() == Backend::Avx2 {
                    // SAFETY: AVX2 and FMA were just verified on this CPU.
                    unsafe { avx2::map1(x, out, avx2::$kernel) };
                    let fix: Option<fn(f64) -> f64> = $fix;
                    if let Some(scalar) = fix {
                        // Lanes the trig kernels do not model (|x| > 1e5, inf, NaN).
                        for (o, &v) in out.iter_mut().zip(x) {
                            if !(v.abs() <= avx2::TRIG_LIMIT) {
                                *o = scalar(v);
                            }
                        }
                    }
                    return;
                }
            }
            let _ = backend;
            let f: fn(f64) -> f64 = $scalar;
            for (o, &v) in out.iter_mut().zip(x) {
                *o = f(v);
            }
        }
    };
}

unary!(
    /// `out[i] = exp(x[i])`.
    exp, exp_with, f64::exp, exp_kernel, None
);
unary!(
    /// `out[i] = ln(x[i])`.
    ln, ln_with, f64::ln, ln_kernel, None
);
unary!(
    /// `out[i] = log10(x[i])` (not correctly rounded at exact decades).
    log10, log10_with, f64::log10, log10_kernel, None
);
unary!(
    /// `out[i] = sin(x[i])`.
    sin, sin_with, f64::sin, sin_kernel, Some(f64::sin)
);
unary!(
    /// `out[i] = cos(x[i])`.
    cos, cos_with, f64::cos, cos_kernel, Some(f64::cos)
);
unary!(
    /// `out[i] = tan(x[i])`.
    tan, tan_with, f64::tan, tan_kernel, Some(f64::tan)
);

/// `out[i] = x[i]^y[i]` — `exp(y·ln x)` on the AVX2 backend for `x > 0`;
/// non-positive bases, zero/infinite/NaN operands and everything else the
/// vector path does not model (`x ≤ 0`, `y = 0`, non-finite) go through
/// `f64::powf` per lane, so the special cases are libm's exactly.
pub fn pow(x: &[f64], y: &[f64], out: &mut [f64]) {
    pow_with(backend(), x, y, out)
}

/// [`pow`] on an explicit [`Backend`].
pub fn pow_with(backend: Backend, x: &[f64], y: &[f64], out: &mut [f64]) {
    assert!(
        x.len() == y.len() && x.len() == out.len(),
        "fastmath: input/output length mismatch"
    );
    #[cfg(target_arch = "x86_64")]
    {
        if backend == Backend::Avx2 && avx2_available() == Backend::Avx2 {
            // SAFETY: AVX2 and FMA were just verified on this CPU.
            unsafe { avx2::map2(x, y, out) };
            return;
        }
    }
    let _ = backend;
    for ((o, &a), &b) in out.iter_mut().zip(x).zip(y) {
        *o = a.powf(b);
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    // ── Slice drivers ───────────────────────────────────────────────────────
    //
    // Every element, tail included, runs through the vector kernel (the tail is
    // padded with 1.0 — harmless for every kernel), so a value's result is
    // independent of its position and of the slice length.

    type Kernel = unsafe fn(__m256d) -> __m256d;

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn map1(x: &[f64], out: &mut [f64], kernel: Kernel) {
        let n = x.len();
        let mut i = 0;
        while i + 4 <= n {
            let v = _mm256_loadu_pd(x.as_ptr().add(i));
            _mm256_storeu_pd(out.as_mut_ptr().add(i), kernel(v));
            i += 4;
        }
        if i < n {
            let mut pad = [1.0f64; 4];
            pad[..n - i].copy_from_slice(&x[i..]);
            let r = kernel(_mm256_loadu_pd(pad.as_ptr()));
            _mm256_storeu_pd(pad.as_mut_ptr(), r);
            out[i..].copy_from_slice(&pad[..n - i]);
        }
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn map2(x: &[f64], y: &[f64], out: &mut [f64]) {
        let n = x.len();
        let mut i = 0;
        while i + 4 <= n {
            let r = pow_kernel(
                _mm256_loadu_pd(x.as_ptr().add(i)),
                _mm256_loadu_pd(y.as_ptr().add(i)),
            );
            _mm256_storeu_pd(out.as_mut_ptr().add(i), r);
            i += 4;
        }
        if i < n {
            let (mut px, mut py) = ([1.0f64; 4], [1.0f64; 4]);
            px[..n - i].copy_from_slice(&x[i..]);
            py[..n - i].copy_from_slice(&y[i..]);
            let r = pow_kernel(_mm256_loadu_pd(px.as_ptr()), _mm256_loadu_pd(py.as_ptr()));
            _mm256_storeu_pd(px.as_mut_ptr(), r);
            out[i..].copy_from_slice(&px[..n - i]);
        }
        // The vector path models x > 0, finite, y ≠ 0 finite; libm owns the rest.
        for ((o, &a), &b) in out.iter_mut().zip(x).zip(y) {
            if !(a > 0.0 && a.is_finite() && b != 0.0 && b.is_finite()) {
                *o = a.powf(b);
            }
        }
    }

    // ── Constants ───────────────────────────────────────────────────────────

    const LOG2E: f64 = 1.442_695_040_888_963_4;
    // fdlibm's split of ln 2: LN2_HI has 21 trailing zero bits, so k·LN2_HI is
    // exact for |k| < 2^21.
    const LN2_HI: f64 = 6.931_471_803_691_238_164_90e-01;
    const LN2_LO: f64 = 1.908_214_929_270_587_700_02e-10;
    const SQRT2: f64 = std::f64::consts::SQRT_2;
    pub const TRIG_LIMIT: f64 = 1.0e5;

    #[inline(always)]
    unsafe fn set(v: f64) -> __m256d {
        _mm256_set1_pd(v)
    }

    /// `mask ? a : b`.
    #[inline(always)]
    unsafe fn select(mask: __m256d, a: __m256d, b: __m256d) -> __m256d {
        _mm256_blendv_pd(b, a, mask)
    }

    /// `2^k` for integer-valued `k` (as f64) with |k| ≤ 1100, as a double:
    /// biased exponent shifted into place.
    #[inline(always)]
    unsafe fn pow2_from_i64(k: __m256i) -> __m256d {
        let biased = _mm256_add_epi64(k, _mm256_set1_epi64x(1023));
        _mm256_castsi256_pd(_mm256_slli_epi64(biased, 52))
    }

    // ── exp ─────────────────────────────────────────────────────────────────

    /// e^x: x = k·ln2 + r, |r| ≤ ln2/2, e^r by a degree-13 Taylor/Horner FMA
    /// chain (|r|^14/14! < 5e-18), scaled by 2^k in two steps so results in the
    /// subnormal range round once.
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn exp_kernel(x: __m256d) -> __m256d {
        let nan = _mm256_cmp_pd::<_CMP_UNORD_Q>(x, x);
        let xc = _mm256_min_pd(_mm256_max_pd(x, set(-745.5)), set(709.9));
        let kf = _mm256_round_pd::<{ _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC }>(
            _mm256_mul_pd(xc, set(LOG2E)),
        );
        let r = _mm256_fnmadd_pd(kf, set(LN2_HI), xc);
        let r = _mm256_fnmadd_pd(kf, set(LN2_LO), r);

        let mut p = set(1.0 / 6_227_020_800.0); // 1/13!
        for inv_fact in [
            1.0 / 479_001_600.0,
            1.0 / 39_916_800.0,
            1.0 / 3_628_800.0,
            1.0 / 362_880.0,
            1.0 / 40_320.0,
            1.0 / 5_040.0,
            1.0 / 720.0,
            1.0 / 120.0,
            1.0 / 24.0,
            1.0 / 6.0,
            0.5,
            1.0,
            1.0,
        ] {
            p = _mm256_fmadd_pd(p, r, set(inv_fact));
        }

        let k32 = _mm256_cvtpd_epi32(kf);
        let k = _mm256_cvtepi32_epi64(k32);
        let k1 = _mm_srai_epi32::<1>(k32);
        let k1 = _mm256_cvtepi32_epi64(k1);
        let k2 = _mm256_sub_epi64(k, k1);
        let res = _mm256_mul_pd(_mm256_mul_pd(p, pow2_from_i64(k1)), pow2_from_i64(k2));
        select(nan, x, res)
    }

    // ── ln / log10 ──────────────────────────────────────────────────────────

    /// ln x: x = m·2^e with m ∈ [√½, √2); ln m = 2·atanh(s), s = (m−1)/(m+1),
    /// by the series 2s·(1 + z/3 + z²/5 + … + z¹⁰/21), z = s² ≤ 0.0295.
    #[target_feature(enable = "avx2,fma")]
    unsafe fn ln_parts(x: __m256d) -> (__m256d, __m256d) {
        let zero = _mm256_setzero_pd();
        // Subnormals: scale by 2^54 and take the exponent back out.
        let sub = _mm256_and_pd(
            _mm256_cmp_pd::<_CMP_LT_OQ>(x, set(f64::MIN_POSITIVE)),
            _mm256_cmp_pd::<_CMP_GT_OQ>(x, zero),
        );
        let xs = select(sub, _mm256_mul_pd(x, set(18_014_398_509_481_984.0)), x);
        let bits = _mm256_castpd_si256(xs);
        let exp_bits = _mm256_and_si256(_mm256_srli_epi64::<52>(bits), _mm256_set1_epi64x(0x7ff));
        // i64 → f64 for small non-negative values via the 2^52 magic number.
        let magic = _mm256_set1_epi64x(0x4330_0000_0000_0000);
        let e = _mm256_sub_pd(
            _mm256_castsi256_pd(_mm256_or_si256(exp_bits, magic)),
            set(4_503_599_627_370_496.0 + 1023.0),
        );
        let mant = _mm256_or_si256(
            _mm256_and_si256(bits, _mm256_set1_epi64x(0x000f_ffff_ffff_ffff)),
            _mm256_set1_epi64x(0x3ff0_0000_0000_0000),
        );
        let m = _mm256_castsi256_pd(mant);
        let big = _mm256_cmp_pd::<_CMP_GT_OQ>(m, set(SQRT2));
        let m = select(big, _mm256_mul_pd(m, set(0.5)), m);
        let e = _mm256_add_pd(e, _mm256_and_pd(big, set(1.0)));
        let e = _mm256_sub_pd(e, _mm256_and_pd(sub, set(54.0)));

        let f = _mm256_sub_pd(m, set(1.0));
        let s = _mm256_div_pd(f, _mm256_add_pd(set(2.0), f));
        let z = _mm256_mul_pd(s, s);
        let mut q = set(1.0 / 21.0);
        for inv in [
            1.0 / 19.0,
            1.0 / 17.0,
            1.0 / 15.0,
            1.0 / 13.0,
            1.0 / 11.0,
            1.0 / 9.0,
            1.0 / 7.0,
            1.0 / 5.0,
            1.0 / 3.0,
        ] {
            q = _mm256_fmadd_pd(q, z, set(inv));
        }
        // ln m = 2s + 2s·z·q
        let two_s = _mm256_add_pd(s, s);
        let ln_m = _mm256_fmadd_pd(_mm256_mul_pd(two_s, z), q, two_s);
        (e, ln_m)
    }

    /// Specials shared by ln and log10: x < 0 → NaN, 0 → −inf, +inf → +inf.
    #[inline(always)]
    unsafe fn ln_specials(x: __m256d, res: __m256d) -> __m256d {
        let zero = _mm256_setzero_pd();
        let nan = _mm256_cmp_pd::<_CMP_UNORD_Q>(x, x);
        let neg = _mm256_cmp_pd::<_CMP_LT_OQ>(x, zero);
        let is_zero = _mm256_cmp_pd::<_CMP_EQ_OQ>(x, zero);
        let is_inf = _mm256_cmp_pd::<_CMP_EQ_OQ>(x, set(f64::INFINITY));
        let r = select(is_inf, set(f64::INFINITY), res);
        let r = select(is_zero, set(f64::NEG_INFINITY), r);
        let r = select(neg, set(f64::NAN), r);
        select(nan, x, r)
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn ln_kernel(x: __m256d) -> __m256d {
        let (e, ln_m) = ln_parts(x);
        // e·ln2 in two pieces so the (exact) high part carries no rounding.
        let hi = _mm256_mul_pd(e, set(LN2_HI));
        let lo = _mm256_fmadd_pd(e, set(LN2_LO), ln_m);
        ln_specials(x, _mm256_add_pd(hi, lo))
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn log10_kernel(x: __m256d) -> __m256d {
        let (e, ln_m) = ln_parts(x);
        // log10 x = e·log10(2) + ln(m)·log10(e).
        let r = _mm256_fmadd_pd(e, set(std::f64::consts::LOG10_2), _mm256_mul_pd(ln_m, set(std::f64::consts::LOG10_E)));
        ln_specials(x, r)
    }

    // ── sin / cos / tan ─────────────────────────────────────────────────────

    // fdlibm's 33-bit pieces of π/2: k·PIO2_n is exact for |k| < 2^20.
    const TWO_OVER_PI: f64 = 0.636_619_772_367_581_4;
    const PIO2_1: f64 = 1.570_796_326_734_125_614_17e+00;
    const PIO2_2: f64 = 6.077_100_506_303_965_976_60e-11;
    const PIO2_3: f64 = 2.022_266_248_711_166_455_80e-21;
    const PIO2_3T: f64 = 8.478_427_660_368_899_569_97e-32;

    /// (sin r, cos r, quadrant parity masks) of x reduced to [−π/4, π/4].
    /// Returns (s, c, swap, neg_sin, neg_cos) with the masks as all-ones lanes.
    #[target_feature(enable = "avx2,fma")]
    unsafe fn sincos_reduced(x: __m256d) -> (__m256d, __m256d, __m256d, __m256d, __m256d) {
        let kf = _mm256_round_pd::<{ _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC }>(
            _mm256_mul_pd(x, set(TWO_OVER_PI)),
        );
        let r = _mm256_fnmadd_pd(kf, set(PIO2_1), x);
        let r = _mm256_fnmadd_pd(kf, set(PIO2_2), r);
        let r = _mm256_fnmadd_pd(kf, set(PIO2_3), r);
        let r = _mm256_fnmadd_pd(kf, set(PIO2_3T), r);
        let z = _mm256_mul_pd(r, r);

        // sin r = r + r·z·(−1/3! + z/5! − z²/7! + …), through r^19.
        let mut ps = set(-1.0 / 121_645_100_408_832_000.0); // −1/19!
        for c in [
            1.0 / 355_687_428_096_000.0,     // 1/17!
            -1.0 / 1_307_674_368_000.0,      // −1/15!
            1.0 / 6_227_020_800.0,           // 1/13!
            -1.0 / 39_916_800.0,             // −1/11!
            1.0 / 362_880.0,                 // 1/9!
            -1.0 / 5_040.0,                  // −1/7!
            1.0 / 120.0,                     // 1/5!
            -1.0 / 6.0,                      // −1/3!
        ] {
            ps = _mm256_fmadd_pd(ps, z, set(c));
        }
        let s = _mm256_fmadd_pd(_mm256_mul_pd(r, z), ps, r);

        // cos r = 1 + z·(−1/2! + z/4! − …), through r^20.
        let mut pc = set(1.0 / 2_432_902_008_176_640_000.0); // 1/20!
        for c in [
            -1.0 / 6_402_373_705_728_000.0,  // −1/18!
            1.0 / 20_922_789_888_000.0,      // 1/16!
            -1.0 / 87_178_291_200.0,         // −1/14!
            1.0 / 479_001_600.0,             // 1/12!
            -1.0 / 3_628_800.0,              // −1/10!
            1.0 / 40_320.0,                  // 1/8!
            -1.0 / 720.0,                    // −1/6!
            1.0 / 24.0,                      // 1/4!
            -0.5,                            // −1/2!
        ] {
            pc = _mm256_fmadd_pd(pc, z, set(c));
        }
        let c = _mm256_fmadd_pd(z, pc, set(1.0));

        // Quadrant q = k mod 4 from the low bits of k.
        let k = _mm256_cvtepi32_epi64(_mm256_cvtpd_epi32(kf));
        let one = _mm256_set1_epi64x(1);
        let two = _mm256_set1_epi64x(2);
        let zero = _mm256_setzero_si256();
        let swap = _mm256_castsi256_pd(_mm256_cmpgt_epi64(_mm256_and_si256(k, one), zero));
        let neg_sin = _mm256_castsi256_pd(_mm256_cmpgt_epi64(_mm256_and_si256(k, two), zero));
        let kp1 = _mm256_add_epi64(k, one);
        let neg_cos = _mm256_castsi256_pd(_mm256_cmpgt_epi64(_mm256_and_si256(kp1, two), zero));
        (s, c, swap, neg_sin, neg_cos)
    }

    #[inline(always)]
    unsafe fn flip(mask: __m256d, v: __m256d) -> __m256d {
        _mm256_xor_pd(v, _mm256_and_pd(mask, set(-0.0)))
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn sin_kernel(x: __m256d) -> __m256d {
        let (s, c, swap, neg_sin, _) = sincos_reduced(x);
        // sin(±0) = ±0: keep the zero's sign, which the polynomial would lose.
        let is_zero = _mm256_cmp_pd::<_CMP_EQ_OQ>(x, _mm256_setzero_pd());
        select(is_zero, x, flip(neg_sin, select(swap, c, s)))
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn cos_kernel(x: __m256d) -> __m256d {
        let (s, c, swap, _, neg_cos) = sincos_reduced(x);
        flip(neg_cos, select(swap, s, c))
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn tan_kernel(x: __m256d) -> __m256d {
        let (s, c, swap, _, _) = sincos_reduced(x);
        // even quadrants: s/c; odd quadrants: −c/s.
        let num = select(swap, c, s);
        let den = select(swap, s, c);
        let is_zero = _mm256_cmp_pd::<_CMP_EQ_OQ>(x, _mm256_setzero_pd());
        select(is_zero, x, flip(swap, _mm256_div_pd(num, den)))
    }

    // ── pow ─────────────────────────────────────────────────────────────────

    /// x^y = exp(y·ln x) for x > 0. The product y·ln x is formed with an FMA
    /// residual so the exponential sees `hi + lo` and the multiplication's
    /// rounding does not get amplified by the full |y·ln x|.
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn pow_kernel(x: __m256d, y: __m256d) -> __m256d {
        let (e, ln_m) = ln_parts(x);
        // ln x = e·ln2 + ln m, kept as hi + lo.
        let hi_a = _mm256_mul_pd(e, set(LN2_HI));
        let lo_a = _mm256_fmadd_pd(e, set(LN2_LO), ln_m);
        let l_hi = _mm256_add_pd(hi_a, lo_a);
        let l_lo = _mm256_add_pd(_mm256_sub_pd(hi_a, l_hi), lo_a);
        // t = y·(l_hi + l_lo) as t_hi + t_lo.
        let t_hi = _mm256_mul_pd(y, l_hi);
        let t_lo = _mm256_fmadd_pd(y, l_lo, _mm256_fmsub_pd(y, l_hi, t_hi));
        // exp(t_hi + t_lo) = exp(t_hi)·(1 + t_lo).
        let base = exp_kernel(t_hi);
        _mm256_fmadd_pd(base, t_lo, base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distance in units in the last place between two finite doubles.
    fn ulps(a: f64, b: f64) -> f64 {
        if a == b {
            return 0.0;
        }
        if !a.is_finite() || !b.is_finite() {
            return f64::INFINITY;
        }
        let key = |v: f64| {
            let bits = v.to_bits() as i64;
            if bits < 0 { i64::MIN - bits } else { bits }
        };
        (key(a) as i128 - key(b) as i128).unsigned_abs() as f64
    }

    /// SplitMix64: reproducible samples without a dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        /// Uniform in [0, 1).
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.unit()
        }
    }

    fn have_avx2() -> bool {
        avx2_available() == Backend::Avx2
    }

    fn max_ulp(
        name: &str,
        inputs: &[f64],
        fast: impl Fn(Backend, &[f64], &mut [f64]),
        exact: impl Fn(f64) -> f64,
    ) -> f64 {
        let mut out = vec![0.0; inputs.len()];
        fast(Backend::Avx2, inputs, &mut out);
        let mut worst = 0.0f64;
        for (&x, &got) in inputs.iter().zip(&out) {
            let want = exact(x);
            let d = ulps(got, want);
            assert!(d <= 4.0, "{name}({x:e}) = {got:e}, libm {want:e}: {d} ulp");
            worst = worst.max(d);
        }
        worst
    }

    #[test]
    fn scalar_backend_is_libm_exactly() {
        let xs: Vec<f64> = (-50..50).map(|i| i as f64 * 0.37).collect();
        let mut out = vec![0.0; xs.len()];
        exp_with(Backend::Scalar, &xs, &mut out);
        assert!(xs.iter().zip(&out).all(|(x, o)| o.to_bits() == x.exp().to_bits()));
        sin_with(Backend::Scalar, &xs, &mut out);
        assert!(xs.iter().zip(&out).all(|(x, o)| o.to_bits() == x.sin().to_bits()));
        let pos: Vec<f64> = xs.iter().map(|x| x.abs() + 0.1).collect();
        pow_with(Backend::Scalar, &pos, &xs, &mut out);
        assert!(pos.iter().zip(&xs).zip(&out).all(|((a, b), o)| o.to_bits() == a.powf(*b).to_bits()));
    }

    #[test]
    fn exp_stays_within_four_ulp() {
        if !have_avx2() {
            return;
        }
        let mut rng = Rng(1);
        let mut xs: Vec<f64> = (0..200_000).map(|_| rng.range(-700.0, 700.0)).collect();
        xs.extend((0..50_000).map(|_| rng.range(-1.0, 1.0)));
        xs.extend([0.0, -0.0, 1.0, -1.0, 709.78, -745.0, -708.4, 1e-300, -1e-300]);
        let worst = max_ulp("exp", &xs, exp_with, f64::exp);
        eprintln!("exp worst = {worst} ulp");
    }

    #[test]
    fn exp_handles_overflow_underflow_and_specials() {
        if !have_avx2() {
            return;
        }
        let xs = [
            710.0, 800.0, f64::INFINITY, f64::NEG_INFINITY, -746.0, -800.0, f64::NAN, 709.782712893384,
            -744.0, -745.1, 0.0, -0.0,
        ];
        let mut out = vec![0.0; xs.len()];
        exp_with(Backend::Avx2, &xs, &mut out);
        for (&x, &got) in xs.iter().zip(&out) {
            let want = x.exp();
            if want.is_nan() {
                assert!(got.is_nan(), "exp({x}) = {got}");
            } else if want == 0.0 || want.is_infinite() {
                assert_eq!(got, want, "exp({x})");
            } else {
                // Subnormal results: accept 1 ulp of the subnormal grid.
                assert!(ulps(got, want) <= 4.0, "exp({x}) = {got:e} vs {want:e}");
            }
        }
    }

    #[test]
    fn ln_and_log10_stay_within_four_ulp() {
        if !have_avx2() {
            return;
        }
        let mut rng = Rng(2);
        let mut xs: Vec<f64> =
            (0..200_000).map(|_| 10f64.powf(rng.range(-300.0, 300.0))).collect();
        xs.extend((0..100_000).map(|_| rng.range(0.5, 2.0)));
        xs.extend((0..20_000).map(|_| 1.0 + rng.range(-1e-6, 1e-6)));
        xs.extend([1.0, 2.0, 10.0, 0.5, f64::MIN_POSITIVE, 1e-310, 5e-324, f64::MAX]);
        let w1 = max_ulp("ln", &xs, ln_with, f64::ln);
        // log10 is not correctly rounded; give it the same 4-ulp budget but
        // skip the exact-decade inputs whose libm value is exactly integral.
        let xs10: Vec<f64> = xs.iter().copied().filter(|x| x.log10().fract() != 0.0).collect();
        let w2 = max_ulp("log10", &xs10, log10_with, f64::log10);
        eprintln!("ln worst = {w1} ulp, log10 worst = {w2} ulp");
    }

    #[test]
    fn log_specials_follow_libm() {
        if !have_avx2() {
            return;
        }
        let xs = [0.0, -0.0, -1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, 1.0];
        for f in [ln_with, log10_with] {
            let mut out = vec![0.0; xs.len()];
            f(Backend::Avx2, &xs, &mut out);
            assert_eq!(out[0], f64::NEG_INFINITY);
            assert_eq!(out[1], f64::NEG_INFINITY);
            assert!(out[2].is_nan() && out[4].is_nan() && out[5].is_nan());
            assert_eq!(out[3], f64::INFINITY);
            assert_eq!(out[6], 0.0);
        }
    }

    #[test]
    fn trig_stays_within_four_ulp() {
        if !have_avx2() {
            return;
        }
        let mut rng = Rng(3);
        let mut xs: Vec<f64> = (0..200_000).map(|_| rng.range(-1000.0, 1000.0)).collect();
        xs.extend((0..50_000).map(|_| rng.range(-1e5, 1e5)));
        xs.extend((0..50_000).map(|_| rng.range(-0.8, 0.8)));
        // Near multiples of π/2, where reduction cancels hardest.
        for k in 1..2000 {
            let base = k as f64 * std::f64::consts::FRAC_PI_2;
            xs.push(base);
            xs.push(base + 1e-9);
            xs.push(base - 1e-9);
        }
        let ws = max_ulp("sin", &xs, sin_with, f64::sin);
        let wc = max_ulp("cos", &xs, cos_with, f64::cos);
        // tan amplifies the reduction's rounding near the poles only through
        // its own value, which libm shares — compare away from |cos| ≈ 0.
        let xt: Vec<f64> = xs.iter().copied().filter(|x| x.cos().abs() > 1e-6).collect();
        let wt = max_ulp("tan", &xt, tan_with, f64::tan);
        eprintln!("sin {ws} / cos {wc} / tan {wt} ulp");
    }

    #[test]
    fn trig_beyond_the_reduction_limit_and_specials_use_libm() {
        if !have_avx2() {
            return;
        }
        let xs = [1e5 + 1.0, -3e7, 1e300, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, 0.0, -0.0];
        for (f, g) in [
            (sin_with as fn(Backend, &[f64], &mut [f64]), f64::sin as fn(f64) -> f64),
            (cos_with, f64::cos),
            (tan_with, f64::tan),
        ] {
            let mut out = vec![0.0; xs.len()];
            f(Backend::Avx2, &xs, &mut out);
            for (&x, &got) in xs.iter().zip(&out) {
                let want = g(x);
                assert!(
                    (got.is_nan() && want.is_nan()) || got.to_bits() == want.to_bits(),
                    "f({x:e}) = {got:e}, libm {want:e}"
                );
            }
        }
    }

    #[test]
    fn pow_error_is_bounded_by_the_documented_budget() {
        if !have_avx2() {
            return;
        }
        let mut rng = Rng(4);
        let n = 200_000;
        let xs: Vec<f64> = (0..n).map(|_| 10f64.powf(rng.range(-3.0, 3.0))).collect();
        let ys: Vec<f64> = (0..n).map(|_| rng.range(-6.0, 6.0)).collect();
        let mut out = vec![0.0; n];
        pow_with(Backend::Avx2, &xs, &ys, &mut out);
        let mut worst = 0.0f64;
        for i in 0..n {
            let want = xs[i].powf(ys[i]);
            let budget = 4.0 + 2.0 * (ys[i] * xs[i].ln()).abs();
            let d = ulps(out[i], want);
            assert!(d <= budget, "pow({:e}, {:e}) = {:e} vs {:e}: {d} ulp > {budget}", xs[i], ys[i], out[i], want);
            worst = worst.max(d);
        }
        eprintln!("pow worst = {worst} ulp");
    }

    #[test]
    fn pow_special_cases_are_libms() {
        let xs = [0.0, -2.0, 2.0, f64::INFINITY, f64::NAN, 1.0, 5.0, -8.0];
        let ys = [3.0, 2.0, 0.0, 2.0, 2.0, f64::NAN, f64::INFINITY, 1.0 / 3.0];
        for b in [Backend::Avx2, Backend::Scalar] {
            let mut out = vec![0.0; xs.len()];
            pow_with(b, &xs, &ys, &mut out);
            for i in 0..xs.len() {
                let want = xs[i].powf(ys[i]);
                assert!(
                    (out[i].is_nan() && want.is_nan()) || out[i].to_bits() == want.to_bits(),
                    "{b:?}: pow({}, {}) = {} vs {}",
                    xs[i], ys[i], out[i], want
                );
            }
        }
    }

    #[test]
    fn results_do_not_depend_on_position_or_batch_length() {
        if !have_avx2() {
            return;
        }
        let mut rng = Rng(5);
        let xs: Vec<f64> = (0..41).map(|_| rng.range(-30.0, 30.0)).collect();
        let mut whole = vec![0.0; xs.len()];
        exp_with(Backend::Avx2, &xs, &mut whole);
        for (i, &x) in xs.iter().enumerate() {
            let mut one = [0.0];
            exp_with(Backend::Avx2, &[x], &mut one);
            assert_eq!(one[0].to_bits(), whole[i].to_bits(), "element {i} changed with batch shape");
        }
        for len in 0..9 {
            let mut part = vec![0.0; len];
            exp_with(Backend::Avx2, &xs[..len], &mut part);
            assert!(part.iter().zip(&whole).all(|(a, b)| a.to_bits() == b.to_bits()));
        }
    }

    #[test]
    fn the_default_backend_matches_detection() {
        let b = backend();
        if std::env::var_os("RSPICE_NO_SIMD").is_some() {
            assert_eq!(b, Backend::Scalar);
        } else {
            assert_eq!(b, avx2_available());
        }
        let mut out = [0.0; 3];
        exp(&[0.0, 1.0, -1.0], &mut out);
        assert!((out[1] - std::f64::consts::E).abs() < 1e-15);
    }

    /// `cargo test --release -p circuitsim --lib fastmath_throughput -- --ignored --nocapture`
    #[test]
    #[ignore = "timing probe, not a correctness test"]
    fn fastmath_throughput() {
        use std::time::Instant;
        let n = 1 << 16;
        let reps = 200;
        let mut rng = Rng(9);
        let xs: Vec<f64> = (0..n).map(|_| rng.range(-20.0, 20.0)).collect();
        let pos: Vec<f64> = xs.iter().map(|x| x.abs() + 0.01).collect();
        let mut out = vec![0.0; n];
        type K = fn(Backend, &[f64], &mut [f64]);
        let cases: [(&str, K, &[f64]); 5] = [
            ("exp", exp_with, &xs),
            ("ln", ln_with, &pos),
            ("log10", log10_with, &pos),
            ("sin", sin_with, &xs),
            ("tan", tan_with, &xs),
        ];
        for (name, f, input) in cases {
            let mut t = [0.0f64; 2];
            for (slot, b) in [Backend::Scalar, Backend::Avx2].into_iter().enumerate() {
                let start = Instant::now();
                for _ in 0..reps {
                    f(b, input, &mut out);
                    std::hint::black_box(&out);
                }
                t[slot] = start.elapsed().as_secs_f64() / (reps * n) as f64 * 1e9;
            }
            eprintln!("{name:6} scalar {:6.2} ns/elem  avx2 {:6.2} ns/elem  ({:.1}x)", t[0], t[1], t[0] / t[1]);
        }
    }
}
