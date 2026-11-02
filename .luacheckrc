std = "lua51"
max_line_length = 150
files["packaging/lightroom/*.lua"] = {globals = {"import", "WIN_ENV"}}
files["packaging/tests/*.lua"] = {std = "lua51", globals = {"import", "WIN_ENV"}}
