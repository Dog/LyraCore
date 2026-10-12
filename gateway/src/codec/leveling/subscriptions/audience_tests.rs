use super::*;
use crate::stdb::bindings::CharacterExplored;
use crate::stdb::world_view::test_support::viewer;
use crate::world::Outbound;

fn explored(area_bit: i32, area_id: u32, experience: u32) -> CharacterExplored {
    CharacterExplored {
        id: 1,
        character_guid: 9001,
        area_bit,
        area_id,
        experience,
    }
}

#[test]
fn exploration_replays_fog_before_discovery_and_reconnects_stay_quiet() {
    let viewer = viewer(1, 9001);
    viewer.explored.lock().unwrap().seed([40]);

    let resident = explored(40, 87, 120);
    let resident_out = exploration_outbound_for_word(&viewer, &resident, 1, 1 << 8);
    let (fog_opcode, fog_body) = crate::codec::build_explored_zones_values(9001, 1, 1 << 8);
    match resident_out.as_slice() {
        [Outbound::Raw { opcode, body }] => {
            assert_eq!((*opcode, body), (fog_opcode, &fog_body));
        }
        _ => panic!("a seeded area restores fog without discovery feedback"),
    }

    let fresh = explored(41, 88, 121);
    let fresh_out = exploration_outbound_for_word(&viewer, &fresh, 1, (1 << 8) | (1 << 9));
    let (new_fog_opcode, new_fog_body) =
        crate::codec::build_explored_zones_values(9001, 1, (1 << 8) | (1 << 9));
    let (discovery_opcode, discovery_body) =
        crate::codec::build_exploration_experience_raw(88, 121);
    match fresh_out.as_slice() {
        [Outbound::Raw {
            opcode: fog_opcode,
            body: fog_body,
        }, Outbound::Raw {
            opcode: toast_opcode,
            body: toast_body,
        }] => {
            assert_eq!((*fog_opcode, fog_body), (new_fog_opcode, &new_fog_body));
            assert_eq!(
                (*toast_opcode, toast_body),
                (discovery_opcode, &discovery_body)
            );
        }
        _ => panic!("fog VALUES must precede discovery feedback for a new area"),
    }
}
