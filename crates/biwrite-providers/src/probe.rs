//! The short requests of a squeeze and of keeping a key alive, different
//! every time so they repeat no text and no prompt. Half are translations:
//! a few words in a LaTeX shape, asked for in one of many wordings, with
//! one of several short system prompts. The other half are small tasks
//! with a short answer (describe an animal, multiply two numbers, spell a
//! word backwards, a three-line poem), each drawn from many wordings and
//! filled with words picked at random. The words, section titles and
//! formulas come from the sample papers TeX Live ships (the ACM, IEEE and
//! Elsevier samples, `sample2e.tex`, `small2e.tex` and the `amsmath` test
//! file). Each request is a handful of words and a short prompt, as cheap
//! as the fixed translation it replaces.

use crate::config::{Effort, ProviderConfig};
use crate::keys::random_u64;

const WORDS: &str = include_str!("probe/words.txt");
const TITLES: &str = include_str!("probe/titles.txt");
const MATHS: &str = include_str!("probe/maths.txt");

/// Longest text, in characters.
pub(crate) const MAX_CHARS: usize = 120;

/// System prompts, each asking for the translation alone (a short answer).
const SYSTEMS: &[&str] = &[
    "Translate the user's text into Chinese. Reply with the translation only.",
    "You are a translator for academic papers. Answer with the Chinese translation and nothing else.",
    "You translate English LaTeX into Chinese. Keep commands and math as they are. Output only the result.",
    "Reply in Chinese with a translation of the text, nothing more.",
    "你是学术论文翻译助手，只输出中文译文。",
    "把用户给出的内容译成中文，保留 LaTeX 命令，只回复译文。",
    "你负责英译中，回答只包含译文。",
    "Translator. Chinese output only.",
    "You help with paper writing. When asked to translate, give only the translation.",
    "将英文翻译为简体中文，不要解释。",
    "Keep the answer short: the Chinese translation only.",
    "You are a concise English to Chinese translator.",
];

/// Ways to ask, `{t}` standing for the text.
const ASKS: &[&str] = &[
    "请帮我翻译：{t}",
    "请帮我翻译一下\n{t}",
    "把这些翻译成中文：{t}",
    "把下面这段 LaTeX 翻成中文\n{t}",
    "翻译成中文：{t}",
    "{t}\n\n翻译成中文",
    "{t}\n这个中文怎么说？",
    "帮忙译成中文，保留 LaTeX 命令：{t}",
    "中文翻译：{t}",
    "这段话的中文意思是？\n{t}",
    "英译中：{t}",
    "pls translate {t}",
    "pls translate to chinese: {t}",
    "Translate into Chinese:\n{t}",
    "Please translate the following into Chinese.\n\n{t}",
    "Could you put this into Chinese? {t}",
    "Chinese, please: {t}",
    "{t}\n\n(translate to Chinese)",
    "EN to ZH: {t}",
    "How would you say this in Chinese?\n{t}",
    "translate: {t}",
    "Help me translate this LaTeX snippet into Chinese, keep the commands.\n{t}",
    "Render in Chinese:\n{t}",
    "zh? {t}",
];

fn lines(list: &'static str) -> Vec<&'static str> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

/// System prompts of the small tasks, each asking for a short answer.
const TASK_SYSTEMS: &[&str] = &[
    "Answer in one short line.",
    "Keep it under twenty words.",
    "简短回答，一句话即可。",
    "你是一个有趣的助手，回答尽量简短。",
    "Be playful and brief.",
    "Reply with as few words as you can.",
    "回答不超过二十个字。",
    "You are a cheerful assistant. Short answers only.",
    "Answer briefly, no explanations.",
    "用一句话回答，可以幽默一点。",
];

/// Small tasks in Chinese, `{…}` standing for words drawn at random.
const TASKS_ZH: &[&str] = &[
    "用一句话形容{animal}。",
    "给一只{color}的{animal}起个名字。",
    "{a} 乘 {b} 等于多少？只回答数字。",
    "{a} 加 {b} 等于几？",
    "写一句关于{season}的短诗，不超过十五个字。",
    "说出三种{color}的东西。",
    "把单词 {word} 倒过来拼写。",
    "推荐一个适合{season}的小活动，一句话就好。",
    "用三个 emoji 表示{animal}。",
    "{city} 在哪个国家？",
    "把 {n} 写成罗马数字。",
    "用一句话夸夸{object}。",
    "在 {a} 和 {c} 之间随便说一个质数。",
    "{animal}和{animal2}比赛跑步，谁会赢？一句话。",
    "把“{zh}”翻译成英文。",
    "给{food}写一句广告词。",
    "用一个成语形容{weather}。",
    "单词 {word} 有几个字母？",
    "如果{animal}会说话，它的第一句话会是什么？",
    "给{job}起一个有趣的外号。",
    "把 {small} 转成二进制。",
    "{weather}适合听什么歌？说一个歌名就好。",
    "{fruit}和{food}哪个更适合当早餐？一句话。",
    "用五个字描述{place}。",
    "讲一个关于{object}的十字小故事。",
    "说一种以字母 {letter} 开头的英文动物名。",
    "把“{zh}”扩写成一句话。",
    "{n} 是奇数还是偶数？",
    "写一个关于{fruit}的谜语，不要给答案。",
    "{object}和{animal}有什么共同点？一句话。",
    "用 {word} 造一个很短的英文句子。",
    "给{place}写一句欢迎标语。",
];

/// Small tasks in English.
const TASKS_EN: &[&str] = &[
    "Describe a {animal} in one sentence.",
    "Give a {color} {animal} a funny name.",
    "What is {a} times {b}? Reply with the number.",
    "Write a three-line poem about {season}.",
    "Name three {color} things.",
    "Spell '{word}' backwards.",
    "Which country is {city} in?",
    "Write {n} in Roman numerals.",
    "Give one word that rhymes with '{word}'.",
    "Tell a ten-word story about a {animal} and a {object}.",
    "Name a prime number between {a} and {c}.",
    "Roughly how many miles is {n} kilometres?",
    "What sound does a {animal} make? One word.",
    "How many letters are in '{word}'?",
    "Pick an emoji for {season}.",
    "Suggest a slogan for {food}.",
    "Name an animal that starts with {letter}.",
    "What would a {animal} say if it could talk?",
    "Give a nickname to a {job}.",
    "Is {n} odd or even?",
    "Write a riddle about a {fruit}, no answer.",
    "Translate '{word}' into Chinese.",
    "Describe {weather} weather in five words.",
    "Suggest a song title for a {weather} day.",
    "Name something {color} you can eat.",
    "Share a fun fact about the {animal} in one sentence.",
    "Write {a} plus {b} in words.",
    "Invent a holiday for the {object} in one line.",
    "Which is the better breakfast, {fruit} or {food}? One line.",
    "Name a city that starts with {letter}.",
    "Use '{word}' in a very short sentence.",
    "Convert {small} to binary.",
];

/// Openings of a task, often none.
const OPENINGS_ZH: &[&str] = &[
    "",
    "",
    "",
    "",
    "嗨，",
    "随便问问：",
    "考考你：",
    "小问题：",
    "请问，",
    "顺便一问，",
    "来个轻松的：",
    "换个脑子：",
];
const OPENINGS_EN: &[&str] = &[
    "",
    "",
    "",
    "",
    "Quick one: ",
    "Hey, ",
    "Just curious: ",
    "Small question: ",
    "Riddle time: ",
    "Fun question: ",
    "Here is one: ",
    "Brain break: ",
];
/// Ways to answer, often none.
const STYLES_ZH: &[&str] = &[
    "",
    "",
    "",
    "用古风的语气。",
    "像天气预报员一样说。",
    "带一个 emoji。",
    "用东北话说。",
    "控制在十个字以内。",
    "像小学生一样回答。",
    "语气夸张一点。",
    "像诗人一样回答。",
    "像相声演员一样回答。",
    "最后加一个感叹号。",
    "像导游一样介绍。",
    "用反问句回答。",
];
const STYLES_EN: &[&str] = &[
    "",
    "",
    "",
    "Answer like a pirate.",
    "Add one emoji.",
    "In exactly seven words.",
    "Sound very dramatic.",
    "Answer like a weather forecaster.",
    "Rhyme if you can.",
    "Answer as a haiku.",
    "Be extra polite.",
    "Answer like a sports commentator.",
    "End with an exclamation mark.",
    "Answer like a tour guide.",
    "Answer with a question.",
];

const ANIMALS_ZH: &[&str] = &[
    "熊猫",
    "企鹅",
    "猫头鹰",
    "海豚",
    "刺猬",
    "狐狸",
    "长颈鹿",
    "蜗牛",
    "袋鼠",
    "柯基",
    "骆驼",
    "章鱼",
    "仓鼠",
    "树懒",
    "鹦鹉",
    "斑马",
    "海獭",
    "考拉",
    "松鼠",
    "河马",
    "水母",
    "猎豹",
    "火烈鸟",
    "浣熊",
    "柴犬",
    "海龟",
    "羊驼",
    "蜂鸟",
    "北极熊",
    "小熊猫",
];
const ANIMALS_EN: &[&str] = &[
    "panda",
    "penguin",
    "owl",
    "dolphin",
    "hedgehog",
    "fox",
    "giraffe",
    "snail",
    "kangaroo",
    "corgi",
    "camel",
    "octopus",
    "hamster",
    "sloth",
    "parrot",
    "zebra",
    "otter",
    "koala",
    "squirrel",
    "hippo",
    "jellyfish",
    "cheetah",
    "flamingo",
    "raccoon",
    "shiba inu",
    "sea turtle",
    "alpaca",
    "hummingbird",
    "polar bear",
    "red panda",
];
const OBJECTS_ZH: &[&str] = &[
    "台灯",
    "雨伞",
    "咖啡杯",
    "键盘",
    "拖鞋",
    "闹钟",
    "书包",
    "风筝",
    "望远镜",
    "自行车",
    "保温杯",
    "耳机",
    "橡皮",
    "地球仪",
    "灯笼",
    "订书机",
    "仙人掌盆栽",
    "旧收音机",
    "魔方",
    "热气球",
    "便利贴",
];
const OBJECTS_EN: &[&str] = &[
    "lamp",
    "umbrella",
    "coffee mug",
    "keyboard",
    "slipper",
    "alarm clock",
    "backpack",
    "kite",
    "telescope",
    "bicycle",
    "teapot",
    "headphone",
    "eraser",
    "globe",
    "lantern",
    "stapler",
    "cactus pot",
    "old radio",
    "Rubik's cube",
    "hot-air balloon",
    "sticky note",
];
const FRUITS_ZH: &[&str] = &[
    "西瓜", "芒果", "草莓", "菠萝", "香蕉", "樱桃", "荔枝", "柠檬", "葡萄", "橙子",
];
const FRUITS_EN: &[&str] = &[
    "watermelon",
    "mango",
    "strawberry",
    "pineapple",
    "banana",
    "cherry",
    "lychee",
    "lemon",
    "grape",
    "orange",
];
const FOODS_ZH: &[&str] = &[
    "煎饼果子",
    "小笼包",
    "拉面",
    "饺子",
    "火锅",
    "豆浆",
    "烤红薯",
    "蛋炒饭",
    "披萨",
    "汉堡",
];
const FOODS_EN: &[&str] = &[
    "dumplings",
    "ramen",
    "pancakes",
    "pizza",
    "hot pot",
    "sushi",
    "tacos",
    "noodles",
    "porridge",
    "bagels",
];
const COLORS_ZH: &[&str] = &[
    "红色", "蓝色", "绿色", "紫色", "金色", "银色", "粉色", "橙色", "黑色", "白色",
];
const COLORS_EN: &[&str] = &[
    "red", "blue", "green", "purple", "golden", "silver", "pink", "orange", "black", "white",
];
const SEASONS_ZH: &[&str] = &[
    "春天", "夏天", "秋天", "冬天", "雨季", "深夜", "清晨", "周末",
];
const SEASONS_EN: &[&str] = &[
    "spring",
    "summer",
    "autumn",
    "winter",
    "a rainy afternoon",
    "midnight",
    "early morning",
    "the weekend",
];
const WEATHER_ZH: &[&str] = &["晴天", "下雨天", "大雪天", "大风天", "雾天", "雷雨天"];
const WEATHER_EN: &[&str] = &["sunny", "rainy", "snowy", "windy", "foggy", "stormy"];
const JOBS_ZH: &[&str] = &["程序员", "厨师", "邮递员", "宇航员", "图书管理员", "消防员"];
const JOBS_EN: &[&str] = &[
    "programmer",
    "chef",
    "postman",
    "astronaut",
    "librarian",
    "firefighter",
];
const PLACES_ZH: &[&str] = &[
    "图书馆",
    "海边",
    "山顶",
    "地铁站",
    "咖啡馆",
    "菜市场",
    "实验室",
    "操场",
];
const CITIES: &[&str] = &[
    "Paris",
    "Kyoto",
    "Cairo",
    "Lima",
    "Oslo",
    "Nairobi",
    "Hanoi",
    "Lisbon",
    "Toronto",
    "Seoul",
    "Sydney",
    "Prague",
    "Istanbul",
    "Havana",
    "Reykjavik",
    "Chengdu",
    "Harbin",
    "Marrakesh",
    "Cusco",
    "Tbilisi",
    "Valparaiso",
    "Bergen",
    "Hobart",
    "Kraków",
    "Luang Prabang",
    "Zanzibar",
];
const PHRASES_ZH: &[&str] = &[
    "你好世界",
    "今天天气不错",
    "我喜欢猫",
    "一起去散步吧",
    "学习使我快乐",
    "早点休息",
    "加油",
    "慢慢来",
    "周末愉快",
    "多喝热水",
];

/// A small random source (xorshift), seeded per text.
struct Dice(u64);

impl Dice {
    fn new() -> Self {
        Self(random_u64() | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n.max(1) as u64).unwrap_or(0)
    }

    fn pick(&mut self, list: &[&'static str]) -> &'static str {
        list[self.below(list.len())]
    }

    /// `low` to `high` words (inclusive).
    fn words(&mut self, list: &[&'static str], low: usize, high: usize) -> Vec<&'static str> {
        let n = low + self.below(high - low + 1);
        (0..n).map(|_| self.pick(list)).collect()
    }
}

fn capital(word: &str) -> String {
    let mut c = word.chars();
    c.next().map_or(String::new(), |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

fn sentence(words: &[&str]) -> String {
    format!("{}.", capital(&words.join(" ")))
}

fn title(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| capital(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `config` for short translations: little reasoning, so a few words cost
/// a few tokens (a provider that sends no effort still sends none).
pub fn config(config: &ProviderConfig, effort: Effort) -> ProviderConfig {
    let mut config = config.clone();
    if config.effort != Effort::Default {
        config.effort = effort;
    }
    config
}

/// [`config`] at low effort, for the test of a provider or a key.
pub fn light_config(config: &ProviderConfig) -> ProviderConfig {
    self::config(config, Effort::Low)
}

/// The system prompt and the user's message of one short request: a
/// translation or a small task, half and half.
pub(crate) fn prompt() -> (String, String) {
    let mut d = Dice::new();
    if d.below(2) == 0 {
        return task(&mut d);
    }
    translation(&mut d)
}

/// The system prompt and question of a test of a provider or a key: one
/// of the small tasks, drawn as for a keep-alive.
pub fn test_question() -> (String, String) {
    task(&mut Dice::new())
}

/// A small task with a short answer, in Chinese or English.
fn task(d: &mut Dice) -> (String, String) {
    let words = lines(WORDS);
    let zh = d.below(2) == 0;
    let template = if zh {
        d.pick(TASKS_ZH)
    } else {
        d.pick(TASKS_EN)
    };
    let (animals, objects, fruits, foods, colors, seasons, weather, jobs) = if zh {
        (
            ANIMALS_ZH, OBJECTS_ZH, FRUITS_ZH, FOODS_ZH, COLORS_ZH, SEASONS_ZH, WEATHER_ZH, JOBS_ZH,
        )
    } else {
        (
            ANIMALS_EN, OBJECTS_EN, FRUITS_EN, FOODS_EN, COLORS_EN, SEASONS_EN, WEATHER_EN, JOBS_EN,
        )
    };
    let a = 2 + d.below(60);
    let b = 2 + d.below(40);
    let animal = d.pick(animals);
    let mut animal2 = d.pick(animals);
    if animal2 == animal {
        animal2 =
            animals[(animals.iter().position(|x| *x == animal).unwrap_or(0) + 1) % animals.len()];
    }
    let letter = char::from(b'A' + d.below(26) as u8).to_string();
    let user = template
        .replace("{animal2}", animal2)
        .replace("{animal}", animal)
        .replace("{object}", d.pick(objects))
        .replace("{fruit}", d.pick(fruits))
        .replace("{food}", d.pick(foods))
        .replace("{color}", d.pick(colors))
        .replace("{season}", d.pick(seasons))
        .replace("{weather}", d.pick(weather))
        .replace("{job}", d.pick(jobs))
        .replace("{place}", d.pick(PLACES_ZH))
        .replace("{city}", d.pick(CITIES))
        .replace("{zh}", d.pick(PHRASES_ZH))
        .replace("{word}", d.pick(&words))
        .replace("{letter}", &letter)
        .replace("{a}", &a.to_string())
        .replace("{b}", &b.to_string())
        .replace("{c}", &(a + 10 + d.below(30)).to_string())
        .replace("{small}", &(1 + d.below(255)).to_string())
        .replace("{n}", &(1 + d.below(1999)).to_string());
    let (opening, style) = if zh {
        (d.pick(OPENINGS_ZH), d.pick(STYLES_ZH))
    } else {
        (d.pick(OPENINGS_EN), d.pick(STYLES_EN))
    };
    let joined = match (style.is_empty(), zh) {
        (true, _) => format!("{opening}{user}"),
        (false, true) => format!("{opening}{user}{style}"),
        (false, false) => format!("{opening}{user} {style}"),
    };
    (d.pick(TASK_SYSTEMS).to_owned(), joined)
}

/// A short translation of a few words in a LaTeX shape.
fn translation(d: &mut Dice) -> (String, String) {
    let text = text();
    let system = SYSTEMS[d.below(SYSTEMS.len())].to_owned();
    let one_line = !text.contains('\n');
    let wrapped = match d.below(8) {
        0 if one_line => format!("\"{text}\""),
        1 if one_line => format!("“{text}”"),
        2 if one_line => format!("「{text}」"),
        3 if one_line => format!("`{text}`"),
        4 => format!("```latex\n{text}\n```"),
        5 => format!("```\n{text}\n```"),
        6 => format!("<text>\n{text}\n</text>"),
        _ => text,
    };
    let user = ASKS[d.below(ASKS.len())].replace("{t}", &wrapped);
    (system, user)
}

/// A few words from the lists, in one of several LaTeX shapes.
pub(crate) fn text() -> String {
    let (words, titles, maths) = (lines(WORDS), lines(TITLES), lines(MATHS));
    let mut d = Dice::new();
    let w = d.words(&words, 2, 5);
    let out = match d.below(16) {
        0 => w.join(" "),
        1 => sentence(&w),
        2 => w.join(", "),
        3 => format!("\\section{{{}}}", d.pick(&titles)),
        4 => format!("\\subsection{{{}}}", title(&w[..2])),
        5 => format!("\\textbf{{{}}}: {}", capital(w[0]), sentence(&w[1..])),
        6 => format!("\\emph{{{}}} {}", w[0], w[1..].join(" ")),
        7 => format!(
            "\\begin{{itemize}}\n\\item {}\n\\item {}\n\\end{{itemize}}",
            w[0],
            w[1..].join(" ")
        ),
        8 => format!("\\caption{{{}}}", sentence(&w)),
        9 => format!(
            "{} ${}$ {}.",
            capital(w[0]),
            d.pick(&maths),
            w[1..].join(" ")
        ),
        10 => format!("${}$ {}", d.pick(&maths), w.join(" ")),
        11 => format!(
            "{} \\\\",
            w.iter().map(|x| capital(x)).collect::<Vec<_>>().join(" & ")
        ),
        12 => format!("\\paragraph{{{}}} {}", sentence(&w[..1]), sentence(&w[1..])),
        13 => format!("{} ({})", w[1..].join(" "), w[0]),
        14 => format!("\\footnote{{{}}}", sentence(&w)),
        _ => d.pick(&titles).to_owned(),
    };
    out.chars().take(MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn the_lists_hold_words_titles_and_formulas() {
        assert!(lines(WORDS).len() > 300);
        assert!(lines(TITLES).len() > 20);
        assert!(lines(MATHS).len() > 60);
        assert!(
            lines(WORDS)
                .iter()
                .all(|w| w.chars().all(|c| c.is_ascii_lowercase()))
        );
    }

    #[test]
    fn prompts_vary_in_wording_and_stay_short() {
        let prompts: Vec<(String, String)> = (0..1200).map(|_| prompt()).collect();
        let systems: HashSet<&String> = prompts.iter().map(|(s, _)| s).collect();
        assert_eq!(
            systems.len(),
            SYSTEMS.len() + TASK_SYSTEMS.len(),
            "every system prompt turns up"
        );
        let opening: HashSet<String> = prompts
            .iter()
            .map(|(_, u)| u.chars().take(6).collect())
            .collect();
        assert!(opening.len() > 20, "{} openings", opening.len());
        for (system, user) in &prompts {
            assert!(!system.is_empty() && !user.is_empty());
            assert!(!user.contains("{t}"));
            assert!(user.chars().count() < MAX_CHARS + 90, "{user}");
        }
        // Chinese and English asks both turn up.
        assert!(prompts.iter().any(|(_, u)| u.contains("翻译")));
        assert!(
            prompts
                .iter()
                .any(|(_, u)| u.to_lowercase().contains("translate"))
        );
    }

    #[test]
    fn small_tasks_are_varied_filled_in_and_short() {
        let mut d = Dice::new();
        let tasks: Vec<(String, String)> = (0..1000).map(|_| task(&mut d)).collect();
        let distinct: HashSet<&String> = tasks.iter().map(|(_, u)| u).collect();
        assert!(distinct.len() > 960, "{} distinct of 1000", distinct.len());
        for (system, user) in &tasks {
            assert!(TASK_SYSTEMS.contains(&system.as_str()));
            assert!(
                !user.contains('{') && !user.contains('}'),
                "unfilled: {user}"
            );
            assert!(user.chars().count() < 120, "{user}");
        }
        let wordings: HashSet<String> = tasks
            .iter()
            .map(|(_, u)| u.chars().take(4).collect())
            .collect();
        assert!(wordings.len() > 30, "{} openings", wordings.len());
        assert!(tasks.iter().any(|(_, u)| u.contains("乘")), "Chinese tasks");
        assert!(
            tasks.iter().any(|(_, u)| u.contains("Roman")),
            "English tasks"
        );
        // Half of all requests are tasks, half translations.
        let all: Vec<(String, String)> = (0..1000).map(|_| prompt()).collect();
        let tasks = all
            .iter()
            .filter(|(s, _)| TASK_SYSTEMS.contains(&s.as_str()))
            .count();
        assert!((400..=600).contains(&tasks), "{tasks} tasks of 1000");
    }

    /// A few requests to read (`--ignored --nocapture`).
    #[test]
    #[ignore = "prints samples"]
    fn print_samples() {
        for _ in 0..16 {
            let (system, user) = prompt();
            println!("[{system}] {}", user.replace('\n', " / "));
        }
    }

    #[test]
    fn short_translations_ask_for_little_reasoning() {
        let mut c = ProviderConfig {
            id: "p".into(),
            name: "P".into(),
            kind: crate::config::ProviderKind::OpenaiCompatible,
            base_url: "https://api.example.com/v1".into(),
            model: "m".into(),
            temperature: 0.0,
            effort: Effort::High,
            wire_api: crate::config::WireApi::Responses,
            service_tier: None,
            key_concurrency: None,
            max_retries: None,
            long_context: None,
        };
        assert_eq!(light_config(&c).effort, Effort::Low);
        assert_eq!(light_config(&c).model, "m", "the provider's own model");
        assert_eq!(
            config(&c, Effort::Medium).effort,
            Effort::Medium,
            "as the author chose"
        );
        assert_eq!(config(&c, Effort::XHigh).model, "m");
        c.effort = Effort::Default;
        assert_eq!(
            config(&c, Effort::High).effort,
            Effort::Default,
            "none sent stays none"
        );
    }

    #[test]
    fn texts_are_short_varied_and_balanced() {
        let texts: Vec<String> = (0..400).map(|_| text()).collect();
        let distinct: HashSet<&String> = texts.iter().collect();
        assert!(distinct.len() > 350, "{} distinct", distinct.len());
        for t in &texts {
            assert!(!t.trim().is_empty());
            assert!(t.chars().count() <= MAX_CHARS, "{t}");
        }
        // Whole texts keep their braces and dollars paired (one cut at
        // the limit may not).
        let whole: Vec<&String> = texts
            .iter()
            .filter(|t| t.chars().count() < MAX_CHARS)
            .collect();
        for t in &whole {
            assert_eq!(t.matches('{').count(), t.matches('}').count(), "{t}");
            assert_eq!(t.matches('$').count() % 2, 0, "{t}");
        }
        // Several shapes turn up.
        let shapes = ["\\section{", "\\item ", "$", " & ", "\\caption{", ", "];
        for s in shapes {
            assert!(texts.iter().any(|t| t.contains(s)), "no text with {s}");
        }
    }
}
