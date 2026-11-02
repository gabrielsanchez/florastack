local branding = require "Branding"

return {
    LrSdkVersion = 6.0,
    LrSdkMinimumVersion = 6.0,
    LrToolkitIdentifier = branding.app_id .. ".lightroom",
    LrPluginName = branding.name,
    LrExportServiceProvider = {
        title = branding.name,
        file = "ExportServiceProvider.lua",
    },
    VERSION = {major = 0, minor = 1, revision = 0, build = 1},
}
