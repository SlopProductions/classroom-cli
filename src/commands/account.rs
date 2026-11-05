use crate::Ctx;
use crate::config::Config;
use crate::session;
use anyhow::Result;
use console::style;

pub fn login(ctx: &mut Ctx, from: Option<&str>) -> Result<()> {
    let email = session::login(ctx.cfg.browser.as_deref(), from, ctx.account.as_deref())?.email;
    if ctx.cfg.active_account.is_none() {
        ctx.cfg.active_account = Some(email.clone());
        ctx.cfg.save()?;
    }
    println!(
        "{} Logged in as {}",
        style("✓").green(),
        style(&email).bold()
    );
    if ctx.cfg.active_account.as_deref() != Some(email.as_str()) {
        println!("To use it by default: class auth use {email}");
    }
    Ok(())
}

pub fn list(ctx: &Ctx) -> Result<()> {
    let all = session::list()?;
    if all.is_empty() {
        println!("No accounts. Log in with: class auth login");
    }
    for s in all {
        let browser = s
            .browser
            .as_deref()
            .and_then(|b| b.rsplit('/').next())
            .unwrap_or("?")
            .to_string();
        let info = style(format!(
            "({browser}, session from {})",
            &s.saved_at[..10.min(s.saved_at.len())]
        ))
        .dim();
        if ctx.cfg.active_account.as_deref() == Some(&s.email) {
            println!("{} {} {info}", style("*").green(), style(&s.email).bold());
        } else {
            println!("  {} {info}", s.email);
        }
    }
    Ok(())
}

pub fn use_account(ctx: &mut Ctx, query: &str) -> Result<()> {
    let email = session::find(query)?.email;
    ctx.cfg.active_account = Some(email.clone());
    ctx.cfg.save()?;
    println!("{} Active account: {email}", style("✓").green());
    Ok(())
}

pub fn logout(ctx: &mut Ctx, query: &str) -> Result<()> {
    let email = session::find(query)?.email;
    session::delete(&email)?;
    if ctx.cfg.active_account.as_deref() == Some(&email) {
        ctx.cfg.active_account = session::list()?.into_iter().next().map(|s| s.email);
        ctx.cfg.save()?;
    }
    println!("{} Logged out {email}", style("✓").green());
    Ok(())
}

pub fn config_show(cfg: &Config) -> Result<()> {
    println!("{}", style(format!("# {}", Config::path().display())).dim());
    print!("{}", toml::to_string_pretty(cfg)?);
    Ok(())
}

pub fn config_set(cfg: &mut Config, key: &str, value: &str) -> Result<()> {
    cfg.set(key, value)?;
    cfg.save()?;
    println!("{} {key} = {value}", style("✓").green());
    Ok(())
}
