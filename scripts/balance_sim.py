"""Time to kill one target with the mod's formulas (Fiddlesticks' League values, the mod's multipliers)."""
import math

Q_PCT = [0, .04, .045, .05, .055, .06]; Q_MIN = [0, 40, 60, 80, 100, 120]; Q_CD = [0, 15, 14.5, 14, 13.5, 13]
W_DPS = [0, 60, 90, 120, 150, 180]; W_MISS = [0, .12, .145, .17, .195, .22]; W_CD = [0, 10, 9.5, 9, 8.5, 8]
E_BASE = [0, 70, 105, 140, 175, 210]; E_CD = [0, 10, 9, 8, 7, 6]
R_DPS = [0, 150, 250, 350]; R_CD = [0, 140, 110, 80]


def growth(l):
    l -= 1
    return l * (0.7025 + 0.0175 * l)


def ttk(hp, mr, lvl, ranks, ap, haste=0, burn=0.0, pen_pct=0, pen=0, boss=False, cfg=None, dmg_mult=1.0):
    cfg = cfg or {}
    pct_mult = cfg.get("boss_pct", 1.0) if boss else 1.0
    mr_eff = max(0, mr * (1 - pen_pct) - pen)
    m = 100 / (100 + mr_eff) * dmg_mult
    q, w, e, r = ranks
    cdq = Q_CD[q] * 100 / (100 + haste) if q else 1e9
    cdw = W_CD[w] * 100 / (100 + haste) if w else 1e9
    cde = E_CD[e] * 100 / (100 + haste) if e else 1e9
    cdr = R_CD[r] * 100 / (100 + haste) if r else 1e9
    ad = 55 + 2.65 * growth(lvl)
    aspd = 0.625 * (1 + 0.0211 * growth(lvl))
    cur, t, dt = hp, 0.0, 0.05
    nq = nw = ne = nr = 0.0
    busy = 0.0
    naa = 0.0
    burn_until = -1
    while cur > 0 and t < 600:
        if busy <= 0:
            if r and t >= nr and hp > 2000:
                # 1.5 s channel + leap, then 5 s storm
                busy = 6.5; nr = t + cdr
                cur -= (R_DPS[r] + 0.5 * ap) * 5 * m * dmg_mult / dmg_mult
                burn_until = t + 8
            elif q and t >= nq:
                pct = Q_PCT[q] + cfg.get("q_ap_pct", 0.0003) * ap
                cur -= max(pct * pct_mult * cur, Q_MIN[q]) * m; nq = t + cdq; busy = 0.6; burn_until = t + 3
            elif w and t >= nw:
                cur -= (W_DPS[w] + 0.45 * ap) * 2 * m
                cur -= W_MISS[w] * pct_mult * (hp - cur) * m
                nw = t + cdw; busy = 2.2; burn_until = t + 5
            elif e and t >= ne:
                cur -= (E_BASE[e] + 0.5 * ap) * cfg.get("e_mult", 0.5) * m; ne = t + cde; busy = 0.5; burn_until = t + 3
            elif t >= naa:
                cur -= ad; naa = t + 1 / aspd
        busy -= dt
        if t < burn_until and burn:
            cur -= burn * (cfg.get("burn_boss", 1.0) if boss else 1.0) * hp * dt * m
        t += dt
    return t


def items_ap(n, rab=False):
    return n * 85 * (1.3 if rab else 1.0)


STAGES = {
    # name: (fiddle level, ranks, ER level, items, haste, liandry+blackfire burn, void pen %, flat pen, blessing, targets [(name, hp, mr, boss)])
    "early (Limgrave, Stormveil)": (6, (2, 2, 1, 1), 30, 1, 0, 0.0, 0, 0, 0,
        [("soldado", 250, 20, False), ("caballero", 900, 40, False), ("Margit", 4174, 60, True), ("Godrick", 6080, 60, True)]),
    "mid (Liurnia, Caelid, Altus)": (12, (5, 4, 2, 2), 75, 3, 15, 0.02, 0, 15, 0,
        [("enemigo", 900, 20, False), ("fuerte", 2500, 40, False), ("Radahn", 9572, 60, True), ("Morgott", 10399, 60, True)]),
    "late (Cumbres, Farum, Haligtree)": (18, (5, 5, 5, 3), 140, 6, 30, 0.035, 0.4, 15, 0,
        [("enemigo", 3000, 20, False), ("fuerte", 6000, 40, False), ("Maliketh", 10600, 60, True), ("Malenia", 33000, 60, True)]),
    "DLC (bendicion 12)": (18, (5, 5, 5, 3), 150, 6, 30, 0.035, 0.4, 15, 12,
        [("enemigo", 4000, 20, False), ("fuerte", 9000, 40, False), ("Rellana", 23000, 60, True), ("Radahn consorte", 36000, 60, True)]),
}


def run(cfg, label):
    print(f"\n=== {label} ===")
    for stage, (lvl, ranks, erl, nitems, haste, burn, pp, fp, bless, targets) in STAGES.items():
        if cfg.get("items_by_level"):
            nitems = min(nitems, max(1, min(6, 1 + (erl - 10) // 22)))
        ap = erl * 4 * cfg.get("ap_rune", 1.0) + items_ap(nitems, nitems >= 4)
        dm = 1 + cfg.get("bless", 0.0) * bless
        out = []
        for name, hp, mr, boss in targets:
            out.append(f"{name} {ttk(hp, mr, lvl, ranks, ap, haste, burn, pp, fp, boss, cfg, dm):.0f}s")
        print(f"{stage:34} AP {ap:5.0f}: " + ", ".join(out))




run({}, "ahora")
run({"q_ap_pct":0.0002,"boss_pct":0.2,"items_by_level":True,"bless":0.05,"ap_rune":0.5,"burn_boss":0.25}, "propuesta")
