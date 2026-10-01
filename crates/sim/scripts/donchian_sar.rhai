fn params() {
    #{
        entry: #{ min: 10, max: 60, step: 2, "default": 20 },
    }
}

fn on_bar(ctx, p) {
    let ch = ctx.donchian("M15", p.entry, 1);
    if ch == () { return 0; }
    let c = ctx.close("M15", 0);
    if c > ch.upper { return 1; }
    if c < ch.lower { return -1; }
    ctx.position
}
