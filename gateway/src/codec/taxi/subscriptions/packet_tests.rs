use super::*;
use crate::codec;
use crate::stdb::bindings::TaxiPassengerSpline;
use crate::world::Outbound;
use std::collections::HashSet;
use std::sync::Mutex;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

fn taxi_row(character_guid: u64) -> TaxiPassengerSpline {
    TaxiPassengerSpline {
        character_guid,
        map_id: 0,
        instance_id: 0,
        grid_x: 0,
        grid_y: 0,
        cell: lyracore_shared::spatial::grid_cell_id(0, 0),
        start_x: 1.0,
        start_y: 2.0,
        start_z: 3.0,
        points: vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        duration_ms: 1_000,
        spline_id: 10,
    }
}

#[test]
fn taxi_spline_reaches_self_and_only_created_observers() {
    let row = taxi_row(42);
    let created = Mutex::new(HashSet::new());
    assert_eq!(taxi_spline_outbound(&created, 42, &row).len(), 1);
    assert!(taxi_spline_outbound(&created, 7, &row).is_empty());

    created.lock().unwrap().insert(42);
    assert_eq!(taxi_spline_outbound(&created, 7, &row).len(), 1);
}

#[test]
fn malformed_taxi_spline_is_not_written_to_a_client() {
    let mut row = taxi_row(42);
    row.points.pop();
    assert!(taxi_spline_outbound(&Mutex::new(HashSet::new()), 42, &row).is_empty());
}

#[test]
fn resident_taxi_spline_is_chained_after_passenger_create() {
    let row = taxi_row(42);
    let created = Mutex::new(HashSet::from([42]));
    let mut outbound = vec![Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(
        codec::build_destroy_object(99),
    ))];

    append_resident_taxi_after_create(&mut outbound, &created, 7, &row);

    assert!(matches!(outbound.first(), Some(Outbound::One(_))));
    assert!(matches!(outbound.get(1), Some(Outbound::Raw { opcode, .. }) if *opcode == 0x00DD));
}
