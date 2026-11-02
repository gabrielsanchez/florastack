-- Original, host-independent CLI bridge; compatible with Lightroom's Lua 5.1.
local Bridge = {}

function Bridge.quote(value, windows)
    if type(value) ~= "string" or value == "" or value:find("[%z\r\n]") then
        error("A command argument is empty or contains a NUL or newline.")
    end
    if windows then
        -- cmd.exe expands these even inside quotes. Refuse instead of silently changing a path.
        if value:find('[%%!"%^]') then
            error('Windows paths and presets must not contain percent, exclamation, double quote or caret characters.')
        end
        -- The Windows argv parser treats backslashes immediately before a closing quote specially.
        return '"' .. value:gsub('(\\+)$', '%1%1') .. '"'
    end
    return "'" .. value:gsub("'", "'\\''") .. "'"
end

function Bridge.check(settings, windows)
    Bridge.quote(settings.executable, windows)
    Bridge.quote(settings.preset, windows)
    if settings.method ~= "pyramid" and settings.method ~= "depthmap" and settings.method ~= "average" then
        error("Choose pyramid, depthmap or average as the stacking method.")
    end
end

function Bridge.join(directory, filename)
    return directory:gsub("[/\\]+$", "") .. "/" .. filename
end

-- Reserve a fresh run folder. Retaining its TIFFs keeps the CLI's saved project and sidecar usable.
-- mkdir must fail when another run has already reserved the directory.
function Bridge.reserve(directory, filename, host)
    local stem = filename:gsub("%.[^.]+$", "")
    for number = 1, 10000 do
        local suffix = number == 1 and "" or ("-" .. number)
        local output = Bridge.join(directory, stem .. "-stacked" .. suffix .. ".tif")
        local inputs = output:gsub("%.tif$", "-inputs")
        Bridge.quote(output, host.windows)
        if not host.exists(output) and not host.exists(output .. ".json") and not host.exists(inputs) then
            if host.mkdir(inputs) then
                return {output = output, inputs = inputs, log = Bridge.join(inputs, "cli.log"), result = Bridge.join(inputs, "result.json")}
            end
            if not host.exists(inputs) then error("Could not create " .. inputs .. ". Check folder permissions and free space.") end
        end
    end
    error("Could not reserve an output beside the originals. Check folder permissions and free space.")
end

function Bridge.command(settings, run, windows)
    Bridge.check(settings, windows)
    local arguments = {settings.executable, "stack", run.inputs, "--preset", settings.preset,
        "--method", settings.method, "--depth", "16", "--out", run.output}
    local quoted = {}
    for _, argument in ipairs(arguments) do
        quoted[#quoted + 1] = Bridge.quote(argument, windows)
    end
    local command = table.concat(quoted, " ") .. " 1>" .. Bridge.quote(run.result, windows) .. " 2>" .. Bridge.quote(run.log, windows)
    -- Each host supplies its own Windows shell entry point (Lightroom and darktable differ).
    return command
end

function Bridge.run(settings, run, host)
    local status = host.execute(Bridge.command(settings, run, host.windows))
    if status ~= 0 then
        error("Stacking failed (exit status " .. tostring(status) .. "). Inputs and diagnostics: " .. run.inputs)
    end
    if not host.exists(run.output) then
        error("The CLI reported success without an output TIFF. Diagnostics: " .. run.inputs)
    end
    return run.output
end

return Bridge
