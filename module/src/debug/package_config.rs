use spacetimedb::{reducer, ReducerContext};

#[reducer]
pub(crate) fn debug_seed_package_config(
    ctx: &ReducerContext,
    package_name: String,
    key: String,
    value: String,
) -> Result<(), String> {
    crate::helpers::require_operator(ctx)?;
    crate::package_config::ensure_package_config_default(ctx, &package_name, &key, &value);
    Ok(())
}
