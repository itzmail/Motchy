//! The whole PC's power draw, for the System page's Power module: an
//! estimate on a desktop (what the CPU package and the graphics card report
//! of their own power, plus [`REST_W`] for the rest of the PC), and the
//! Mac's own sensor on a Mac.

/// The estimate's allowance for what reports no power of its own: the
/// board's chipset and power stages, memory, drives, fans and USB devices.
/// A desktop's usually take 30 to 60 W together.
pub const REST_W: f32 = 40.0;

/// The whole PC's power.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerReading {
    /// What the PC draws (W): the estimate, or the Mac's sensor.
    pub total_w: f32,
    /// What the CPU package and the graphics card report of their own (W).
    pub cpu_w: Option<f32>,
    pub gpu_w: Option<f32>,
    /// Everything else: [`REST_W`] (or, on a Mac, all of it).
    pub rest_w: f32,
}

impl PowerReading {
    /// A desktop: the CPU package, the graphics
    /// card and [`REST_W`]. None when a graphics card of its own doesn't
    /// report its power, since the total would leave out what may be most
    /// of it; a GPU built into the processor is inside the package's reading.
    pub fn estimate(cpu_w: Option<f32>, gpu_w: Option<f32>, dedicated_gpu: bool) -> Option<PowerReading> {
        let cpu = cpu_w?;
        let gpu = if dedicated_gpu { Some(gpu_w?) } else { None };
        Some(PowerReading { total_w: cpu + gpu.unwrap_or(0.0) + REST_W, cpu_w: Some(cpu), gpu_w: gpu, rest_w: REST_W })
    }
}

/// "73 W", "8.5 W".
pub fn fmt_w(w: f32) -> String {
    if w < 9.95 {
        format!("{w:.1} W")
    } else {
        format!("{w:.0} W")
    }
}

/// A round top for a power chart at or above `peak` watts.
pub fn watts_ceiling(peak: f32) -> f32 {
    const TOPS: [f32; 11] = [50.0, 100.0, 150.0, 200.0, 300.0, 400.0, 500.0, 750.0, 1000.0, 1500.0, 2000.0];
    let peak = if peak.is_finite() { peak } else { 0.0 };
    TOPS.into_iter().find(|t| peak <= *t).unwrap_or_else(|| (peak / 500.0).ceil() * 500.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_pc_estimate() {
        // A desktop with a graphics card that reports its power.
        let p = PowerReading::estimate(Some(73.0), Some(91.0), true).unwrap();
        assert_eq!((p.total_w, p.rest_w), (204.0, REST_W));
        // A card that doesn't: no total. A GPU in the processor: the package has it.
        assert_eq!(PowerReading::estimate(Some(73.0), None, true), None);
        let p = PowerReading::estimate(Some(35.0), Some(9.0), false).unwrap();
        assert_eq!((p.total_w, p.gpu_w), (75.0, None));
        assert_eq!(PowerReading::estimate(None, Some(91.0), true), None);
    }

    #[test]
    fn formats_and_chart_tops() {
        assert_eq!(fmt_w(73.4), "73 W");
        assert_eq!(fmt_w(8.46), "8.5 W");
        assert_eq!(watts_ceiling(91.0), 100.0);
        assert_eq!(watts_ceiling(412.0), 500.0);
        assert_eq!(watts_ceiling(1530.0), 2000.0);
        assert_eq!(watts_ceiling(2400.0), 2500.0);
        assert_eq!(watts_ceiling(f32::NAN), 50.0);
    }
}
