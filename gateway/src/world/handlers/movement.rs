/// Ignored without decoding; see docs/benilla-compatibility.md#movement-controls.
pub(crate) fn is_silent_movement_request(opcode: u32) -> bool {
    const CMSG_WORLD_TELEPORT: u32 = 0x0008;
    const CMSG_TELEPORT_TO_UNIT: u32 = 0x0009;
    const MSG_MOVE_TELEPORT_CHEAT: u32 = 0x00C6;
    const MSG_MOVE_TOGGLE_COLLISION_CHEAT: u32 = 0x00D9;
    const CMSG_MOVE_SET_RAW_POSITION: u32 = 0x00E1;
    const MSG_MOVE_TOGGLE_GRAVITY_CHEAT: u32 = 0x02AD;
    const CMSG_MOVE_FALL_RESET: u32 = 0x02CA;
    const CMSG_MINIGAME_MOVE: u32 = 0x02F8;
    matches!(
        opcode,
        CMSG_WORLD_TELEPORT
            | CMSG_TELEPORT_TO_UNIT
            | MSG_MOVE_TELEPORT_CHEAT
            | MSG_MOVE_TOGGLE_COLLISION_CHEAT
            | CMSG_MOVE_SET_RAW_POSITION
            | MSG_MOVE_TOGGLE_GRAVITY_CHEAT
            | CMSG_MOVE_FALL_RESET
            | CMSG_MINIGAME_MOVE
    )
}
