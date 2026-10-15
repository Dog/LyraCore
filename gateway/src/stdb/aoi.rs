use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub(crate) static AOI_RECENTERS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub(crate) struct ViewerGates {
    pub(crate) is_ghost: AtomicBool,
}

impl ViewerGates {
    pub(crate) fn is_ghost(&self) -> bool {
        self.is_ghost.load(Ordering::Relaxed)
    }
}

// Keep the SQL oracle independent of the index; matching it to the implementation voids the test.
#[cfg(test)]
mod frozen_sql_oracle {
    use lyracore_shared::spatial::GridBox;

    pub(crate) const BOX_SCOPED_TABLES: [&str; 4] = [
        "game_world_entity",
        "game_gameobject",
        "game_entity_motion",
        "game_creature_spline",
    ];

    fn table_range_query(
        table: &str,
        map_id: u32,
        instance_id: u64,
        gx_min: i32,
        gx_max: i32,
        gy_min: i32,
        gy_max: i32,
    ) -> String {
        format!(
            "SELECT * FROM {table} WHERE map_id = {map_id} AND instance_id = {instance_id} AND grid_x >= {gx_min} AND grid_x <= {gx_max} AND grid_y >= {gy_min} AND grid_y <= {gy_max}"
        )
    }

    fn cell_queries(
        map_id: u32,
        instance_id: u64,
        cells: impl Iterator<Item = i64>,
    ) -> Vec<String> {
        let cells: Vec<i64> = cells.collect();
        let mut out = Vec::with_capacity(BOX_SCOPED_TABLES.len() * cells.len());
        for table in BOX_SCOPED_TABLES {
            for cell in &cells {
                out.push(format!(
                    "SELECT * FROM {table} WHERE map_id = {map_id} AND instance_id = {instance_id} AND cell = {cell}"
                ));
            }
        }
        out
    }

    pub(crate) fn box_queries(instance_id: u64, box_: &GridBox, per_cell: bool) -> Vec<String> {
        if per_cell {
            return cell_queries(box_.map_id, instance_id, box_.cell_ids());
        }
        let (gx_min, gx_max, gy_min, gy_max) = box_.bounds();
        BOX_SCOPED_TABLES
            .into_iter()
            .map(|table| {
                table_range_query(
                    table,
                    box_.map_id,
                    instance_id,
                    gx_min,
                    gx_max,
                    gy_min,
                    gy_max,
                )
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) use frozen_sql_oracle::{box_queries, BOX_SCOPED_TABLES};

#[cfg(test)]
mod tests {
    use super::*;
    use lyracore_shared::spatial::GridBox;

    #[test]
    fn the_frozen_oracle_still_describes_the_shared_crates_box() {
        let b = GridBox {
            map_id: 0,
            gx: 5,
            gy: 7,
        };
        let qs = box_queries(7, &b, false);
        assert_eq!(qs.len(), BOX_SCOPED_TABLES.len());
        for table in BOX_SCOPED_TABLES {
            assert!(qs.iter().any(|q| q.contains(table)), "{qs:#?}");
        }
        let entity = qs
            .iter()
            .find(|q| q.contains("game_world_entity"))
            .expect("game_world_entity");
        assert!(entity.contains("map_id = 0"));
        assert!(entity.contains("instance_id = 7"));
        assert!(entity.contains("grid_x >= 3") && entity.contains("grid_x <= 7"));
        assert!(entity.contains("grid_y >= 5") && entity.contains("grid_y <= 9"));
    }

    #[test]
    fn the_frozen_oracle_probes_exactly_the_boxs_own_cells() {
        let b = GridBox {
            map_id: 0,
            gx: 5,
            gy: 7,
        };
        let qs = box_queries(0, &b, true);
        let want: Vec<i64> = b.cell_ids().collect();
        assert_eq!(want.len(), 25);
        assert_eq!(qs.len(), BOX_SCOPED_TABLES.len() * want.len());
        for table in BOX_SCOPED_TABLES {
            let got: std::collections::HashSet<String> = qs
                .iter()
                .filter(|q| q.contains(table))
                .map(|q| q[q.rfind("cell = ").expect("a cell term") + 7..].to_string())
                .collect();
            let expect: std::collections::HashSet<String> =
                want.iter().map(|c| c.to_string()).collect();
            assert_eq!(got, expect, "{table} probes the wrong cell set");
        }
    }

    #[test]
    fn viewer_gates_round_trip_the_ghost_flag_exactly() {
        let g = ViewerGates::default();
        assert!(!g.is_ghost());
        g.is_ghost.store(true, Ordering::Relaxed);
        assert!(g.is_ghost());
    }
}
