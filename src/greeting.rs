// Port of Greeting.h / Greeting.c
//
// The C version prints these lines with puts(), so the "%%" sequences are
// printed literally. Kept as-is.

pub fn greeting() {
    static ASCII: [&str; 18] = [
        "                                    ",
        "      .S_sSSs     .S    sSSs    sSSs",
        "     .SS~YS%%b   .SS   d%%SP   d%%SP",
        "     S%S   `S%b  S%S  d%S'    d%S'  ",
        "     S%S    S%S  S%S  S%S     S%S   ",
        "     S%S    d*S  S&S  S&S     S&S   ",
        "     S&S   .S*S  S&S  S&S_Ss  S&S_Ss",
        "     S&S_sdSSS   S&S  S&S~SP  S&S~SP",
        "     S&S~YSY%b   S&S  S&S     S&S   ",
        "     S*S   `S%b  S*S  S*b     S*b   ",
        "     S*S    S%S  S*S  S*S     S*S   ",
        "     S*S    S&S  S*S  S*S     S*S   ",
        "     S*S    SSS  S*S  S*S     S*S   ",
        "     SP          SP   SP      SP    ",
        "     Y           Y    Y       Y     ",
        "                                    ",
        "        straight from the devil     ",
        "                                    ",
    ];

    for line in ASCII.iter() {
        println!("{}", line);
    }
}
