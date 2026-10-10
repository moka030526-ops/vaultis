//! The Summary tab: read-only totals across the vault.

use super::*;

impl GuiApp {
    /// The "Summary" tab: a flat table aggregating every Asset/Liability's approximate value
    /// by owner, split into asset buckets (Real Estate / Before Tax / After Tax) and liability
    /// buckets (Before Tax / After Tax), with per-owner totals + net worth and a grand-total
    /// row. Before Tax = retirement + HSA; After Tax = everything else (records::value_bucket).
    pub(super) fn tab_summary(&mut self, ui: &mut egui::Ui) {
        let accent_c = accent(self.theme);
        ui.add_space(6.0);
        section_heading(ui, "Summary of Assets & Liabilities", accent_c);
        ui.label(
            egui::RichText::new(
                "Aggregated approximate values by owner. Cash = cash/savings/checking; Before Tax = retirement + HSA; After Tax = everything else.",
            )
            .weak()
            .small(),
        );
        ui.add_space(10.0);
        let rows = records::owner_value_summary(self.vault_ref().vault.assets.iter());
        if rows.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("📊").size(28.0).color(accent_c.gamma_multiply(0.7)));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Nothing to summarise yet").strong());
                ui.label(
                    egui::RichText::new("Add records on the Assets and Liabilities tab and their values total up here.")
                        .weak()
                        .small(),
                );
            });
            return;
        }
        // Grand total across all owners.
        let mut total = records::OwnerValueRow { owner: "All owners".to_string(), ..Default::default() };
        for r in &rows {
            total.asset_real_estate += r.asset_real_estate;
            total.asset_cash += r.asset_cash; // BUG FIX: cash was omitted, understating Assets Σ / Net
            total.asset_before_tax += r.asset_before_tax;
            total.asset_after_tax += r.asset_after_tax;
            total.liability += r.liability;
        }
        // A headline row before the table: the three numbers someone opens this tab
        // to find, at a size they can read across a desk, instead of having to pick
        // them out of the bottom-right corner of an eight-column grid.
        ui.horizontal_wrapped(|ui| {
            stat_tile(ui, "Total assets", &crate::fmt_money(total.asset_total()), STAT_GOOD);
            stat_tile(ui, "Total liabilities", &crate::fmt_money(total.liability), STAT_BAD);
            // Net worth is the polarity number, so it takes the sign's color — and the
            // sign is in the text too, never color alone.
            stat_tile(
                ui,
                "Net worth",
                &crate::fmt_money(total.net()),
                if total.net() < 0.0 { STAT_BAD } else { STAT_GOOD },
            );
            stat_tile(ui, "Owners", &rows.len().to_string(), accent_c);
        });
        ui.add_space(12.0);
        egui::ScrollArea::both().auto_shrink([false, false]).id_salt("summary_scroll").show(ui, |ui| {
            egui::Grid::new("summary_grid").striped(true).num_columns(8).spacing([18.0, 6.0]).show(ui, |ui| {
                // Group header: ASSETS over its 5 value columns, LIABILITIES over its 1.
                ui.label("");
                ui.label(egui::RichText::new("ASSETS").strong().small().color(STAT_GOOD));
                ui.label("");
                ui.label("");
                ui.label("");
                ui.label("");
                ui.label(egui::RichText::new("LIABILITIES").strong().small().color(STAT_BAD));
                ui.label("");
                ui.end_row();
                // Column headers (Cash = cash/savings/checking; liabilities are not tax-split).
                for h in ["Owner", "Real Estate", "Cash", "Before Tax", "After Tax", "Assets Σ", "Liability", "Net"] {
                    ui.label(egui::RichText::new(h).strong());
                }
                ui.end_row();
                // One row per owner (monospace amounts so the digits line up).
                for r in &rows {
                    ui.label(egui::RichText::new(r.owner.as_str()).strong());
                    ui.monospace(crate::fmt_money(r.asset_real_estate));
                    ui.monospace(crate::fmt_money(r.asset_cash));
                    ui.monospace(crate::fmt_money(r.asset_before_tax));
                    ui.monospace(crate::fmt_money(r.asset_after_tax));
                    ui.monospace(crate::fmt_money(r.asset_total()));
                    // Liability and Net carry the reserved status colors; the sign is in
                    // the text as well, so the meaning never rests on color alone.
                    ui.label(egui::RichText::new(crate::fmt_money(r.liability)).monospace().color(
                        if r.liability > 0.0 { STAT_BAD } else { ui.visuals().text_color() },
                    ));
                    ui.label(
                        egui::RichText::new(crate::fmt_money(r.net()))
                            .monospace()
                            .color(if r.net() < 0.0 { STAT_BAD } else { STAT_GOOD }),
                    );
                    ui.end_row();
                }
                // Grand-total row (bold).
                ui.label(egui::RichText::new(total.owner.as_str()).strong());
                for v in [
                    total.asset_real_estate,
                    total.asset_cash,
                    total.asset_before_tax,
                    total.asset_after_tax,
                    total.asset_total(),
                    total.liability,
                    total.net(),
                ] {
                    ui.label(egui::RichText::new(crate::fmt_money(v)).strong().monospace());
                }
                ui.end_row();
            });
        });
    }
}

// The two reserved status colors for money. They are NOT part of the accent
// family and are never reused as decoration: green means "this is what is owned",
// red "this is what is owed". Both are readable on the light and the dark
// surfaces of all ten themes.
pub(super) const STAT_GOOD: egui::Color32 = egui::Color32::from_rgb(45, 130, 80);

pub(super) const STAT_BAD: egui::Color32 = egui::Color32::from_rgb(185, 70, 70);

/// A headline figure with its label: the Summary tab's KPI row.
///
/// Label above in secondary ink (never in the value's color — the number carries
/// the meaning), value below at display size. Read-only presentation of numbers
/// the table below already contains.
pub(super) fn stat_tile(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.35)))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_min_width(140.0);
                ui.label(egui::RichText::new(label).weak().small());
                ui.add_space(2.0);
                ui.label(egui::RichText::new(value).size(22.0).strong().color(color));
            });
        });
    ui.add_space(6.0);
}
