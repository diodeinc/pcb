use std::time::Duration;

/// Tracks timing for each pipeline stage
#[derive(Debug, Clone)]
pub struct PipelineTiming {
    pub stage0_input: Option<Duration>,
    pub stage1_transforms: Option<Duration>,
    pub stage2_padstacks: Option<Duration>,
    pub stage3_primitives: Option<Duration>,
    pub stage4_booleans: Option<Duration>,
    pub stage4_5_drills: Option<Duration>,
    pub stage5_composite: Option<Duration>,
    pub stage6_emission: Option<Duration>,
}

impl PipelineTiming {
    pub fn new() -> Self {
        Self {
            stage0_input: None,
            stage1_transforms: None,
            stage2_padstacks: None,
            stage3_primitives: None,
            stage4_booleans: None,
            stage4_5_drills: None,
            stage5_composite: None,
            stage6_emission: None,
        }
    }

    pub fn total(&self) -> Duration {
        [
            self.stage0_input,
            self.stage1_transforms,
            self.stage2_padstacks,
            self.stage3_primitives,
            self.stage4_booleans,
            self.stage4_5_drills,
            self.stage5_composite,
            self.stage6_emission,
        ]
        .iter()
        .filter_map(|&d| d)
        .sum()
    }

    pub fn print_summary(&self) {
        println!("━━━ Pipeline Timing ━━━");
        if let Some(d) = self.stage0_input {
            println!(
                "  Stage 0 (Input):       {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        if let Some(d) = self.stage1_transforms {
            println!(
                "  Stage 1 (Transforms):  {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        if let Some(d) = self.stage2_padstacks {
            println!(
                "  Stage 2 (Padstacks):   {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        if let Some(d) = self.stage3_primitives {
            println!(
                "  Stage 3 (Primitives):  {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        if let Some(d) = self.stage4_booleans {
            println!(
                "  Stage 4 (Booleans):    {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        if let Some(d) = self.stage4_5_drills {
            println!(
                "  Stage 4.5 (Drills):    {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        if let Some(d) = self.stage5_composite {
            println!(
                "  Stage 5 (Composite):   {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        if let Some(d) = self.stage6_emission {
            println!(
                "  Stage 6 (Emission):    {:>8.2}ms",
                d.as_secs_f64() * 1000.0
            );
        }
        println!("  ─────────────────────────────");
        println!(
            "  Total:                 {:>8.2}ms",
            self.total().as_secs_f64() * 1000.0
        );
    }
}

impl Default for PipelineTiming {
    fn default() -> Self {
        Self::new()
    }
}
