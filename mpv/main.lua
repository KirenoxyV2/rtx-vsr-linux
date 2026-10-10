-- RTX Video Super Resolution for mpv — runtime toggle around vsr.vpy.
--
-- Install: link/copy this directory into mpv's scripts dir as `rtx-vsr`
-- (see install/install_mpv.sh). Needs an mpv with the vapoursynth filter.
--
-- Default keys (rebind in input.conf via `script-binding rtx_vsr/<name>`):
--   Alt+v  toggle        VSR on/off
--   Alt+q  cycle-mode    LOW -> MEDIUM -> HIGH -> ULTRA
--
-- Options (script-opts/vsr.conf or --script-opts=vsr-mode=3,vsr-auto=yes):
local o = {
    auto = false,      -- enable on startup
    mode = 2,          -- 1=LOW 2=MEDIUM 3=HIGH 4=ULTRA
    scale = "fit",     -- "fit" = upscale to the display, or a factor like 2
    fmt = "420p10",    -- filter output format: 420p8 | 420p10 | 444p10 | 444p16
    gpu = 0,
    downshift = true,  -- step the mode down when the GPU can't hold the frame rate
    vpy = "",          -- path to vsr.vpy; default: next to this script
}
require("mp.options").read_options(o, "vsr")

local utils = require("mp.utils")
local LABEL = "rtx-vsr"
local MODES = { "LOW", "MEDIUM", "HIGH", "ULTRA" }
local enabled = o.auto
local active = false   -- filter currently in the chain
local settle = 0       -- ignore throughput readings until this time

local function vpy_path()
    if o.vpy ~= "" then return mp.command_native({ "expand-path", o.vpy }) end
    local dir = mp.get_script_directory()
    if not dir then
        dir = utils.split_path(debug.getinfo(1, "S").source:sub(2))
    end
    return utils.join_path(dir, "vsr.vpy")
end

-- mpv's %n%string quoting, so paths with , : = " survive option parsing.
local function q(s) return string.format("%%%d%%%s", #s, s) end

local function remove()
    active = false
    for _, f in ipairs(mp.get_property_native("vf") or {}) do
        if f.label == LABEL then mp.commandv("vf", "remove", "@" .. LABEL) end
    end
end

-- Returns the on-screen status line.
local function apply()
    if not enabled then
        remove()
        return "RTX VSR: off"
    end
    local vp = mp.get_property_native("video-params")
    if not vp or not vp.w then return "RTX VSR: on (no video)" end

    local dw = mp.get_property_number("display-width")
    local dh = mp.get_property_number("display-height")
    local data = { "mode=" .. o.mode, "scale=" .. o.scale, "fmt=" .. o.fmt, "gpu=" .. o.gpu }
    if dw and dh then
        if vp.w >= dw or vp.h >= dh then
            -- Nothing to upscale: keep the filter (and its frame copies) out.
            remove()
            return string.format("RTX VSR: idle (%dx%d source fills the %dx%d display)",
                                 vp.w, vp.h, dw, dh)
        end
        data[#data + 1] = "maxw=" .. dw
        data[#data + 1] = "maxh=" .. dh
    end
    active = true
    settle = mp.get_time() + 5
    mp.commandv("vf", "add", string.format("@%s:vapoursynth=file=%s:user-data=%s",
                                           LABEL, q(vpy_path()), q(table.concat(data, ","))))
    return "RTX VSR: " .. (MODES[o.mode] or o.mode)
end

local function show(msg) mp.osd_message(msg, 3) end

mp.add_key_binding("Alt+v", "toggle", function()
    enabled = not enabled
    show(apply())
end)

mp.add_key_binding("Alt+q", "cycle-mode", function()
    o.mode = o.mode % #MODES + 1
    enabled = true
    show(apply())
end)

-- (Re)apply whenever the decoded size or the display changes: at startup
-- with auto=yes, per file (the next one may already fill the display), and
-- when the window moves to another monitor. Debounced, because on Wayland the
-- display size is a placeholder until the window has been mapped.
local pending
local function refresh()
    if pending then pending:kill() end
    pending = mp.add_timeout(0.3, function()
        pending = nil
        if enabled then apply() end
    end)
end
mp.observe_property("video-params/w", "number", refresh)
mp.observe_property("video-params/h", "number", refresh)
mp.observe_property("display-width", "number", refresh)
mp.observe_property("display-height", "number", refresh)

-- Report the real result once the filter has (re)configured the chain.
mp.observe_property("video-out-params", "native", function(_, out)
    if not enabled or not out or not out.w then return end
    local vp = mp.get_property_native("video-params")
    if vp and vp.w and (out.w ~= vp.w or out.h ~= vp.h) then
        show(string.format("RTX VSR: %s  %dx%d → %dx%d",
                           MODES[o.mode] or o.mode, vp.w, vp.h, out.w, out.h))
    end
end)

-- Auto-downshift: a mode the GPU can't run at the video's frame rate shows up
-- as the VO dropping late frames (and A/V sync drifting), so trade quality
-- for speed. Drops that are expected anyway (video faster than the display)
-- don't count.
mp.register_event("playback-restart", function() settle = mp.get_time() + 3 end)
local slow, last_drops = 0, nil
mp.add_periodic_timer(1, function()
    local drops = mp.get_property_number("frame-drop-count") or 0
    local delta = last_drops and drops - last_drops or 0
    last_drops = drops
    if not (o.downshift and active and o.mode > 1) or mp.get_time() < settle
       or mp.get_property_bool("core-idle") then
        slow = 0
        return
    end
    local want = (mp.get_property_number("container-fps") or 0)
                 * (mp.get_property_number("speed") or 1)
    local shown = mp.get_property_number("display-fps") or want
    slow = (want > 0 and delta - math.max(0, want - shown) > want * 0.05) and slow + 1 or 0
    if slow >= 3 then
        slow = 0
        o.mode = o.mode - 1
        apply()
        show(string.format("RTX VSR: GPU can't hold %.0f fps, stepping down to %s",
                           want, MODES[o.mode]))
    end
end)
