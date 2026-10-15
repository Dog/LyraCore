use spacetimedb::{log, reducer, ReducerContext};

use crate::runtime_script::{run_event, CoreEffects, EntityView, RuntimeScript, ScriptEvent};

#[reducer]
pub(crate) fn debug_run_runtime_script(
    ctx: &ReducerContext,
    script_name: String,
    event: String,
    source: String,
    actor_guid: u64,
    target_guid: u64,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    let event = ScriptEvent {
        name: event,
        actor: EntityView::read(ctx, actor_guid),
        target: EntityView::read(ctx, target_guid),
        ..ScriptEvent::default()
    };
    let Some((diagnostics, compilations)) = crate::runtime_script::with_host(|host| {
        let diagnostics = run_event(
            host,
            &mut CoreEffects { ctx },
            &event,
            &[RuntimeScript {
                name: &script_name,
                source: &source,
            }],
        );
        (diagnostics, host.compilations())
    }) else {
        return Err("the runtime script host is already running a script".to_string());
    };
    match diagnostics.first() {
        Some(diagnostic) => Err(diagnostic.to_string()),
        None => {
            log::info!(
                "debug_run_runtime_script: `{script_name}` on `{}` committed \
                 ({compilations} chunks compiled since this module instance started)",
                event.name
            );
            Ok(())
        }
    }
}
