use super::*;

#[derive(Clone, Debug, Default)]
pub struct CorpseView {
    pub guid: u64,
    pub owner_guid: u64,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub orientation: f32,
    pub display_id: u32,
    pub bytes_1: u32,
    pub bytes_2: u32,
    pub is_bones: bool,
}

pub fn build_corpse_query_response(
    loc: Option<(u32, f32, f32, f32)>,
) -> Result<MSG_CORPSE_QUERY_Server> {
    match loc {
        None => Ok(MSG_CORPSE_QUERY_Server::NotFound),
        Some((map_id, x, y, z)) => {
            let map = Map::try_from(map_id).map_err(|_| anyhow!("invalid corpse map {map_id}"))?;
            Ok(MSG_CORPSE_QUERY_Server::Found {
                corpse_map: map,
                map,
                position: Vector3d { x, y, z },
            })
        }
    }
}

pub fn build_corpse_create_object(corpse: &CorpseView) -> SMSG_UPDATE_OBJECT {
    let (b1a, b1b, b1c, b1d) = unpack4(corpse.bytes_1);
    let (b2a, b2b, b2c, b2d) = unpack4(corpse.bytes_2);
    // A zero display id makes build 5875 dereference a null corpse model.
    let display_id = if corpse.display_id == 0 {
        49
    } else {
        corpse.display_id
    };
    let mask = UpdateCorpse::builder()
        .set_object_guid(Guid::new(corpse.guid))
        .set_object_scale_x(1.0)
        .set_corpse_owner(Guid::new(corpse.owner_guid))
        .set_corpse_display_id(display_id as i32)
        .set_corpse_pos_x(corpse.x)
        .set_corpse_pos_y(corpse.y)
        .set_corpse_pos_z(corpse.z)
        .set_corpse_facing(corpse.orientation)
        .set_corpse_bytes_1(b1a, b1b, b1c, b1d)
        .set_corpse_bytes_2(b2a, b2b, b2c, b2d)
        // CORPSE_FLAG_BONE is 0x01; an ordinary body carries 0x04.
        .set_corpse_flags(if corpse.is_bones { 0x01 } else { 0x04 })
        .finalize();
    let update_flag = MovementBlock_UpdateFlag::empty()
        .set_all(MovementBlock_UpdateFlag_All { unknown1: 1 })
        .set_living(MovementBlock_UpdateFlag_Living::HasPosition {
            orientation: corpse.orientation,
            position: Vector3d {
                x: corpse.x,
                y: corpse.y,
                z: corpse.z,
            },
        });
    SMSG_UPDATE_OBJECT {
        has_transport: 0,
        objects: vec![Object::CreateObject2 {
            guid3: Guid::new(corpse.guid),
            object_type: ObjectType::Corpse,
            movement2: MovementBlock { update_flag },
            mask2: UpdateMask::Corpse(mask),
        }],
    }
}

pub fn build_resurrect_request(caster_guid: u64, caster_name: String) -> SMSG_RESURRECT_REQUEST {
    SMSG_RESURRECT_REQUEST {
        guid: Guid::new(caster_guid),
        name: caster_name,
        player: true,
    }
}

pub(crate) mod subscriptions;
