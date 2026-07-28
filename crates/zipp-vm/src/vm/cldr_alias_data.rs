//! CLDR alias registries and likely subtags — GENERATED, DO NOT EDIT.
//!
//! Regenerate with `python tools/gen_cldr_aliases.py <cldr-root>`; that script
//! documents the exact upstream files. This is the locale-INDEPENDENT half of
//! CLDR: the registries UTS #35 §3.2.1 uses to canonicalize a
//! `unicode_locale_id`, plus the §4.3 likely-subtags table. No translated
//! content, no per-locale patterns — the same bytes in every engine that ships
//! them.
//!
//! PROVENANCE: CLDR 47 (tag `release-47` of unicode-org/cldr) —
//!   `common/supplemental/supplementalMetadata.xml`  (the five alias registries)
//!   `common/supplemental/likelySubtags.xml`         (likely subtags)
//!   `common/bcp47/*.xml`                            (`-u-`/`-t-` type aliases)
//! The same CLDR version node 24.12 / ICU 77.1 carries, so
//! `tools/gen_cldr_aliases.py`'s output can be checked value-by-value against
//! node's ICU (see `cldr_alias.rs`'s tests for the sampled results).
//!
//! Every table is a `&str` of newline-separated records so the rows stay
//! diffable against the XML they came from; `cldr_alias.rs` indexes them once
//! behind a `OnceLock`.

/// `<languageAlias type="…" replacement="…"/>`, `type|replacement`.
/// A type may name a whole `unicode_language_id` (`hy-arevmda`,
/// `und-hepburn-heploc`); `und` in a type matches any language.
pub(crate) static LANGUAGE_ALIAS: &str = "\
    art-lojban|jbo\n\
    i-ami|ami\n\
    i-bnn|bnn\n\
    i-hak|hak\n\
    i-klingon|tlh\n\
    i-lux|lb\n\
    i-navajo|nv\n\
    i-pwn|pwn\n\
    i-tao|tao\n\
    i-tay|tay\n\
    i-tsu|tsu\n\
    no-bok|nb\n\
    no-nyn|nn\n\
    sgn-BE-FR|sfb\n\
    sgn-BE-NL|vgt\n\
    sgn-CH-DE|sgg\n\
    zh-guoyu|zh\n\
    zh-hakka|hak\n\
    zh-min-nan|nan\n\
    zh-xiang|hsn\n\
    en-GB-oed|en-GB-oxendict\n\
    in|id\n\
    iw|he\n\
    ji|yi\n\
    jw|jv\n\
    mo|ro\n\
    scc|sr\n\
    scr|hr\n\
    aam|aas\n\
    adp|dz\n\
    aue|ktz\n\
    ayx|nun\n\
    bgm|bcg\n\
    bjd|drl\n\
    ccq|rki\n\
    cjr|mom\n\
    cka|cmr\n\
    cmk|xch\n\
    coy|pij\n\
    cqu|quh\n\
    drh|mn\n\
    drw|fa-AF\n\
    gav|dev\n\
    gfx|vaj\n\
    ggn|gvr\n\
    gti|nyc\n\
    guv|duz\n\
    hrr|jal\n\
    ibi|opa\n\
    ilw|gal\n\
    jeg|oyb\n\
    kgc|tdf\n\
    kgh|kml\n\
    koj|kwv\n\
    krm|bmf\n\
    ktr|dtp\n\
    kvs|gdj\n\
    kwq|yam\n\
    kxe|tvd\n\
    kzj|dtp\n\
    kzt|dtp\n\
    lii|raq\n\
    lmm|rmx\n\
    meg|cir\n\
    mst|mry\n\
    mwj|vaj\n\
    myt|mry\n\
    nad|xny\n\
    ncp|kdz\n\
    nnx|ngv\n\
    nts|pij\n\
    oun|vaj\n\
    pcr|adx\n\
    pmc|huw\n\
    pmu|phr\n\
    ppa|bfy\n\
    ppr|lcq\n\
    pry|prt\n\
    puz|pub\n\
    sca|hle\n\
    skk|oyb\n\
    tdu|dtp\n\
    thc|tpo\n\
    thx|oyb\n\
    tie|ras\n\
    tkk|twm\n\
    tlw|weo\n\
    tmp|tyj\n\
    tne|kak\n\
    tnf|fa-AF\n\
    tsf|taj\n\
    uok|ema\n\
    xba|cax\n\
    xia|acn\n\
    xkh|waw\n\
    xsj|suj\n\
    ybd|rki\n\
    yma|lrr\n\
    ymt|mtm\n\
    yos|zom\n\
    yuu|yug\n\
    asd|snz\n\
    dit|dif\n\
    llo|ngt\n\
    myd|aog\n\
    nns|nbr\n\
    agp|apf\n\
    ais|ami\n\
    ajt|aeb\n\
    baz|nvo\n\
    bhk|fbl\n\
    bic|bir\n\
    bjq|bzc\n\
    bkb|ebk\n\
    blg|iba\n\
    btb|beb\n\
    daf|dnj\n\
    dap|njz\n\
    djl|dze\n\
    dkl|aqd\n\
    drr|kzk\n\
    dud|uth\n\
    duj|dwu\n\
    dwl|dbt\n\
    elp|amq\n\
    gbc|wny\n\
    ggo|esg\n\
    ggr|gtu\n\
    gio|aou\n\
    gli|kzk\n\
    ill|ilm\n\
    izi|eza\n\
    jar|jgk\n\
    kdv|zkd\n\
    kgd|ncq\n\
    kpp|jkm\n\
    kxl|kru\n\
    kzh|dgl\n\
    lak|ksp\n\
    leg|enl\n\
    mgx|jbk\n\
    mnt|wnn\n\
    mof|xnt\n\
    mwd|dmw\n\
    nbf|nru\n\
    nbx|gll\n\
    nln|azd\n\
    nlr|nrk\n\
    noo|dtd\n\
    nxu|bpp\n\
    pat|kxr\n\
    rmr|emx\n\
    sap|aqt\n\
    sgl|isk\n\
    smd|kmb\n\
    snb|iba\n\
    sul|sgd\n\
    sum|ulw\n\
    tgg|bjp\n\
    thw|ola\n\
    tid|itd\n\
    unp|wro\n\
    wgw|wgb\n\
    wit|nol\n\
    wiw|nwo\n\
    xrq|dmw\n\
    yen|ynq\n\
    yiy|yrm\n\
    zir|scv\n\
    sgn-BR|bzs\n\
    sgn-CO|csn\n\
    sgn-DE|gsg\n\
    sgn-DK|dsl\n\
    sgn-FR|fsl\n\
    sgn-GB|bfi\n\
    sgn-GR|gss\n\
    sgn-IE|isg\n\
    sgn-IT|ise\n\
    sgn-JP|jsl\n\
    sgn-MX|mfs\n\
    sgn-NI|ncs\n\
    sgn-NL|dse\n\
    sgn-NO|nsi\n\
    sgn-PT|psr\n\
    sgn-SE|swl\n\
    sgn-US|ase\n\
    sgn-ZA|sfs\n\
    sgn-ES|ssp\n\
    zh-cmn|zh\n\
    zh-cmn-Hans|zh-Hans\n\
    zh-cmn-Hant|zh-Hant\n\
    zh-gan|gan\n\
    zh-wuu|wuu\n\
    zh-yue|yue\n\
    no-bokmal|nb\n\
    no-nynorsk|nn\n\
    aa-saaho|ssy\n\
    sh|sr-Latn\n\
    cnr|sr-ME\n\
    tl|fil\n\
    aju|jrb\n\
    als|sq\n\
    arb|ar\n\
    ayr|ay\n\
    azj|az\n\
    bcc|bal\n\
    bcl|bik\n\
    bxk|luy\n\
    bxr|bua\n\
    cld|syr\n\
    cmn|zh\n\
    cwd|cr\n\
    dgo|doi\n\
    dhd|mwr\n\
    dik|din\n\
    diq|zza\n\
    lbk|bnc\n\
    ekk|et\n\
    emk|man\n\
    esk|ik\n\
    fat|ak\n\
    fuc|ff\n\
    gaz|om\n\
    gbo|grb\n\
    gno|gon\n\
    gom|kok\n\
    gug|gn\n\
    gya|gba\n\
    hdn|hai\n\
    hea|hmn\n\
    ike|iu\n\
    kmr|ku\n\
    knc|kr\n\
    kng|kg\n\
    kpv|kv\n\
    lvs|lv\n\
    mhr|chm\n\
    mup|raj\n\
    khk|mn\n\
    npi|ne\n\
    ojg|oj\n\
    ory|or\n\
    pbu|ps\n\
    pes|fa\n\
    plt|mg\n\
    pnb|lah\n\
    quz|qu\n\
    rmy|rom\n\
    spy|kln\n\
    src|sc\n\
    swh|sw\n\
    ttq|tmh\n\
    tw|ak\n\
    umu|del\n\
    uzn|uz\n\
    xpe|kpe\n\
    xsl|den\n\
    ydd|yi\n\
    zai|zap\n\
    zsm|ms\n\
    zyb|za\n\
    him|srx\n\
    mnk|man\n\
    bh|bho\n\
    cls|sa\n\
    prs|fa-AF\n\
    swc|sw-CD\n\
    aar|aa\n\
    abk|ab\n\
    ave|ae\n\
    afr|af\n\
    aka|ak\n\
    amh|am\n\
    arg|an\n\
    ara|ar\n\
    asm|as\n\
    ava|av\n\
    aym|ay\n\
    aze|az\n\
    bak|ba\n\
    bel|be\n\
    bul|bg\n\
    bih|bho\n\
    bis|bi\n\
    bam|bm\n\
    ben|bn\n\
    bod|bo\n\
    bre|br\n\
    bos|bs\n\
    cat|ca\n\
    che|ce\n\
    cha|ch\n\
    cos|co\n\
    cre|cr\n\
    ces|cs\n\
    chu|cu\n\
    chv|cv\n\
    cym|cy\n\
    dan|da\n\
    deu|de\n\
    div|dv\n\
    dzo|dz\n\
    ewe|ee\n\
    ell|el\n\
    eng|en\n\
    epo|eo\n\
    spa|es\n\
    est|et\n\
    eus|eu\n\
    fas|fa\n\
    ful|ff\n\
    fin|fi\n\
    fij|fj\n\
    fao|fo\n\
    fra|fr\n\
    fry|fy\n\
    gle|ga\n\
    gla|gd\n\
    glg|gl\n\
    grn|gn\n\
    guj|gu\n\
    glv|gv\n\
    hau|ha\n\
    heb|he\n\
    hin|hi\n\
    hmo|ho\n\
    hrv|hr\n\
    hat|ht\n\
    hun|hu\n\
    hye|hy\n\
    her|hz\n\
    ina|ia\n\
    ind|id\n\
    ile|ie\n\
    ibo|ig\n\
    iii|ii\n\
    ipk|ik\n\
    ido|io\n\
    isl|is\n\
    ita|it\n\
    iku|iu\n\
    jpn|ja\n\
    jav|jv\n\
    kat|ka\n\
    kon|kg\n\
    kik|ki\n\
    kua|kj\n\
    kaz|kk\n\
    kal|kl\n\
    khm|km\n\
    kan|kn\n\
    kor|ko\n\
    kau|kr\n\
    kas|ks\n\
    kur|ku\n\
    kom|kv\n\
    cor|kw\n\
    kir|ky\n\
    lat|la\n\
    ltz|lb\n\
    lug|lg\n\
    lim|li\n\
    lin|ln\n\
    lao|lo\n\
    lit|lt\n\
    lub|lu\n\
    lav|lv\n\
    mlg|mg\n\
    mah|mh\n\
    mri|mi\n\
    mkd|mk\n\
    mal|ml\n\
    mon|mn\n\
    mol|ro\n\
    mar|mr\n\
    msa|ms\n\
    mlt|mt\n\
    mya|my\n\
    nau|na\n\
    nob|nb\n\
    nde|nd\n\
    nep|ne\n\
    ndo|ng\n\
    nld|nl\n\
    nno|nn\n\
    nor|no\n\
    nbl|nr\n\
    nav|nv\n\
    nya|ny\n\
    oci|oc\n\
    oji|oj\n\
    orm|om\n\
    ori|or\n\
    oss|os\n\
    pan|pa\n\
    pli|pi\n\
    pol|pl\n\
    pus|ps\n\
    por|pt\n\
    que|qu\n\
    roh|rm\n\
    run|rn\n\
    ron|ro\n\
    rus|ru\n\
    kin|rw\n\
    san|sa\n\
    srd|sc\n\
    snd|sd\n\
    sme|se\n\
    sag|sg\n\
    hbs|sr-Latn\n\
    sin|si\n\
    slk|sk\n\
    slv|sl\n\
    smo|sm\n\
    sna|sn\n\
    som|so\n\
    sqi|sq\n\
    srp|sr\n\
    ssw|ss\n\
    sot|st\n\
    sun|su\n\
    swe|sv\n\
    swa|sw\n\
    tam|ta\n\
    tel|te\n\
    tgk|tg\n\
    tha|th\n\
    tir|ti\n\
    tuk|tk\n\
    tgl|fil\n\
    tsn|tn\n\
    ton|to\n\
    tur|tr\n\
    tso|ts\n\
    tat|tt\n\
    twi|ak\n\
    tah|ty\n\
    uig|ug\n\
    ukr|uk\n\
    urd|ur\n\
    uzb|uz\n\
    ven|ve\n\
    vie|vi\n\
    vol|vo\n\
    wln|wa\n\
    wol|wo\n\
    xho|xh\n\
    yid|yi\n\
    yor|yo\n\
    zha|za\n\
    zho|zh\n\
    zul|zu\n\
    alb|sq\n\
    arm|hy\n\
    baq|eu\n\
    bur|my\n\
    chi|zh\n\
    cze|cs\n\
    dut|nl\n\
    fre|fr\n\
    geo|ka\n\
    ger|de\n\
    gre|el\n\
    ice|is\n\
    mac|mk\n\
    mao|mi\n\
    may|ms\n\
    per|fa\n\
    rum|ro\n\
    slo|sk\n\
    tib|bo\n\
    wel|cy\n\
    cel-gaulish|xtg\n\
    i-default|en-x-i-default\n\
    i-enochian|und-x-i-enochian\n\
    i-mingo|see-x-i-mingo\n\
    zh-min|nan-x-zh-min\n\
    und-aaland|und-AX\n\
    hy-arevmda|hyw\n\
    und-arevmda|und\n\
    und-arevela|und\n\
    und-lojban|und\n\
    und-saaho|und\n\
    und-bokmal|und\n\
    und-nynorsk|und\n\
    und-hakka|und\n\
    und-xiang|und\n\
    und-hepburn-heploc|und-alalc97\n\
    ajp|apc\n\
    kgm|plu\n\
    nom|cbr\n\
    pmk|crr\n\
    prp|gu\n\
    szd|umi\n\
    tmk|tdg\n\
    tpw|tpn\n\
    xss|zko\n\
    zkb|kjh\n\
    ";

/// `<scriptAlias type="…" replacement="…"/>`, `type|replacement`.
pub(crate) static SCRIPT_ALIAS: &str = "\
    Qaai|Zinh\n\
    ";

/// `<territoryAlias type="…" replacement="…"/>`, `type|replacement`.
/// A replacement holding several regions is resolved with likely
/// subtags (UTS #35 §3.2.1); the whole space-separated list is kept.
pub(crate) static TERRITORY_ALIAS: &str = "\
    AN|CW SX BQ\n\
    BU|MM\n\
    CS|RS ME\n\
    CT|KI\n\
    DD|DE\n\
    DY|BJ\n\
    FQ|AQ TF\n\
    FX|FR\n\
    HV|BF\n\
    JT|UM\n\
    MI|UM\n\
    NH|VU\n\
    NQ|AQ\n\
    NT|SA IQ\n\
    PC|FM MH MP PW\n\
    PU|UM\n\
    PZ|PA\n\
    QU|EU\n\
    RH|ZW\n\
    SU|RU AM AZ BY EE GE KZ KG LV LT MD TJ TM UA UZ\n\
    TP|TL\n\
    UK|GB\n\
    VD|VN\n\
    WK|UM\n\
    YD|YE\n\
    YU|RS ME\n\
    ZR|CD\n\
    062|034 143\n\
    172|RU AM AZ BY GE KG KZ MD TJ TM UA UZ\n\
    200|CZ SK\n\
    230|ET\n\
    280|DE\n\
    532|CW SX BQ\n\
    582|FM MH MP PW\n\
    736|SD\n\
    830|JE GG\n\
    886|YE\n\
    890|RS ME SI HR MK BA\n\
    AAA|AA\n\
    ASC|AC\n\
    AND|AD\n\
    ARE|AE\n\
    AFG|AF\n\
    ATG|AG\n\
    AIA|AI\n\
    ALB|AL\n\
    ARM|AM\n\
    ANT|CW SX BQ\n\
    AGO|AO\n\
    ATA|AQ\n\
    ARG|AR\n\
    ASM|AS\n\
    AUT|AT\n\
    AUS|AU\n\
    ABW|AW\n\
    ALA|AX\n\
    AZE|AZ\n\
    BIH|BA\n\
    BRB|BB\n\
    BGD|BD\n\
    BEL|BE\n\
    BFA|BF\n\
    BGR|BG\n\
    BHR|BH\n\
    BDI|BI\n\
    BEN|BJ\n\
    BLM|BL\n\
    BMU|BM\n\
    BRN|BN\n\
    BOL|BO\n\
    BES|BQ\n\
    BRA|BR\n\
    BHS|BS\n\
    BTN|BT\n\
    BUR|MM\n\
    BVT|BV\n\
    BWA|BW\n\
    BLR|BY\n\
    BLZ|BZ\n\
    CAN|CA\n\
    CCK|CC\n\
    COD|CD\n\
    CAF|CF\n\
    COG|CG\n\
    CHE|CH\n\
    CIV|CI\n\
    COK|CK\n\
    CHL|CL\n\
    CMR|CM\n\
    CHN|CN\n\
    COL|CO\n\
    CPT|CP\n\
    CRI|CR\n\
    SCG|RS ME\n\
    CUB|CU\n\
    CPV|CV\n\
    CUW|CW\n\
    CXR|CX\n\
    CYP|CY\n\
    CZE|CZ\n\
    DDR|DE\n\
    DEU|DE\n\
    DGA|DG\n\
    DJI|DJ\n\
    DNK|DK\n\
    DMA|DM\n\
    DOM|DO\n\
    DZA|DZ\n\
    ECU|EC\n\
    EST|EE\n\
    EGY|EG\n\
    ESH|EH\n\
    ERI|ER\n\
    ESP|ES\n\
    ETH|ET\n\
    FIN|FI\n\
    FJI|FJ\n\
    FLK|FK\n\
    FSM|FM\n\
    FRO|FO\n\
    FRA|FR\n\
    FXX|FR\n\
    GAB|GA\n\
    GBR|GB\n\
    GRD|GD\n\
    GEO|GE\n\
    GUF|GF\n\
    GGY|GG\n\
    GHA|GH\n\
    GIB|GI\n\
    GRL|GL\n\
    GMB|GM\n\
    GIN|GN\n\
    GLP|GP\n\
    GNQ|GQ\n\
    GRC|GR\n\
    SGS|GS\n\
    GTM|GT\n\
    GUM|GU\n\
    GNB|GW\n\
    GUY|GY\n\
    HKG|HK\n\
    HMD|HM\n\
    HND|HN\n\
    HRV|HR\n\
    HTI|HT\n\
    HUN|HU\n\
    IDN|ID\n\
    IRL|IE\n\
    ISR|IL\n\
    IMN|IM\n\
    IND|IN\n\
    IOT|IO\n\
    IRQ|IQ\n\
    IRN|IR\n\
    ISL|IS\n\
    ITA|IT\n\
    JEY|JE\n\
    JAM|JM\n\
    JOR|JO\n\
    JPN|JP\n\
    KEN|KE\n\
    KGZ|KG\n\
    KHM|KH\n\
    KIR|KI\n\
    COM|KM\n\
    KNA|KN\n\
    PRK|KP\n\
    KOR|KR\n\
    KWT|KW\n\
    CYM|KY\n\
    KAZ|KZ\n\
    LAO|LA\n\
    LBN|LB\n\
    LCA|LC\n\
    LIE|LI\n\
    LKA|LK\n\
    LBR|LR\n\
    LSO|LS\n\
    LTU|LT\n\
    LUX|LU\n\
    LVA|LV\n\
    LBY|LY\n\
    MAR|MA\n\
    MCO|MC\n\
    MDA|MD\n\
    MNE|ME\n\
    MAF|MF\n\
    MDG|MG\n\
    MHL|MH\n\
    MKD|MK\n\
    MLI|ML\n\
    MMR|MM\n\
    MNG|MN\n\
    MAC|MO\n\
    MNP|MP\n\
    MTQ|MQ\n\
    MRT|MR\n\
    MSR|MS\n\
    MLT|MT\n\
    MUS|MU\n\
    MDV|MV\n\
    MWI|MW\n\
    MEX|MX\n\
    MYS|MY\n\
    MOZ|MZ\n\
    NAM|NA\n\
    NCL|NC\n\
    NER|NE\n\
    NFK|NF\n\
    NGA|NG\n\
    NIC|NI\n\
    NLD|NL\n\
    NOR|NO\n\
    NPL|NP\n\
    NRU|NR\n\
    NTZ|SA IQ\n\
    NIU|NU\n\
    NZL|NZ\n\
    OMN|OM\n\
    PAN|PA\n\
    PER|PE\n\
    PYF|PF\n\
    PNG|PG\n\
    PHL|PH\n\
    PAK|PK\n\
    POL|PL\n\
    SPM|PM\n\
    PCN|PN\n\
    PRI|PR\n\
    PSE|PS\n\
    PRT|PT\n\
    PLW|PW\n\
    PRY|PY\n\
    QAT|QA\n\
    QMM|QM\n\
    QNN|QN\n\
    QPP|QP\n\
    QQQ|QQ\n\
    QRR|QR\n\
    QSS|QS\n\
    QTT|QT\n\
    QUU|EU\n\
    QVV|QV\n\
    QWW|QW\n\
    QXX|QX\n\
    QYY|QY\n\
    QZZ|QZ\n\
    REU|RE\n\
    ROU|RO\n\
    SRB|RS\n\
    RUS|RU\n\
    RWA|RW\n\
    SAU|SA\n\
    SLB|SB\n\
    SYC|SC\n\
    SDN|SD\n\
    SWE|SE\n\
    SGP|SG\n\
    SHN|SH\n\
    SVN|SI\n\
    SJM|SJ\n\
    SVK|SK\n\
    SLE|SL\n\
    SMR|SM\n\
    SEN|SN\n\
    SOM|SO\n\
    SUR|SR\n\
    SSD|SS\n\
    STP|ST\n\
    SUN|RU AM AZ BY EE GE KZ KG LV LT MD TJ TM UA UZ\n\
    SLV|SV\n\
    SXM|SX\n\
    SYR|SY\n\
    SWZ|SZ\n\
    TAA|TA\n\
    TCA|TC\n\
    TCD|TD\n\
    ATF|TF\n\
    TGO|TG\n\
    THA|TH\n\
    TJK|TJ\n\
    TKL|TK\n\
    TLS|TL\n\
    TKM|TM\n\
    TUN|TN\n\
    TON|TO\n\
    TMP|TL\n\
    TUR|TR\n\
    TTO|TT\n\
    TUV|TV\n\
    TWN|TW\n\
    TZA|TZ\n\
    UKR|UA\n\
    UGA|UG\n\
    UMI|UM\n\
    USA|US\n\
    URY|UY\n\
    UZB|UZ\n\
    VAT|VA\n\
    VCT|VC\n\
    VEN|VE\n\
    VGB|VG\n\
    VIR|VI\n\
    VNM|VN\n\
    VUT|VU\n\
    WLF|WF\n\
    WSM|WS\n\
    XAA|XA\n\
    XBB|XB\n\
    XCC|XC\n\
    XDD|XD\n\
    XEE|XE\n\
    XFF|XF\n\
    XGG|XG\n\
    XHH|XH\n\
    XII|XI\n\
    XJJ|XJ\n\
    XKK|XK\n\
    XLL|XL\n\
    XMM|XM\n\
    XNN|XN\n\
    XOO|XO\n\
    XPP|XP\n\
    XQQ|XQ\n\
    XRR|XR\n\
    XSS|XS\n\
    XTT|XT\n\
    XUU|XU\n\
    XVV|XV\n\
    XWW|XW\n\
    XXX|XX\n\
    XYY|XY\n\
    XZZ|XZ\n\
    YMD|YE\n\
    YEM|YE\n\
    MYT|YT\n\
    YUG|RS ME\n\
    ZAF|ZA\n\
    ZMB|ZM\n\
    ZAR|CD\n\
    ZWE|ZW\n\
    ZZZ|ZZ\n\
    958|AA\n\
    020|AD\n\
    784|AE\n\
    004|AF\n\
    028|AG\n\
    660|AI\n\
    008|AL\n\
    051|AM\n\
    530|CW SX BQ\n\
    024|AO\n\
    010|AQ\n\
    032|AR\n\
    016|AS\n\
    040|AT\n\
    036|AU\n\
    533|AW\n\
    248|AX\n\
    031|AZ\n\
    070|BA\n\
    052|BB\n\
    050|BD\n\
    056|BE\n\
    854|BF\n\
    100|BG\n\
    048|BH\n\
    108|BI\n\
    204|BJ\n\
    652|BL\n\
    060|BM\n\
    096|BN\n\
    068|BO\n\
    535|BQ\n\
    076|BR\n\
    044|BS\n\
    064|BT\n\
    104|MM\n\
    074|BV\n\
    072|BW\n\
    112|BY\n\
    084|BZ\n\
    124|CA\n\
    166|CC\n\
    180|CD\n\
    140|CF\n\
    178|CG\n\
    756|CH\n\
    384|CI\n\
    184|CK\n\
    152|CL\n\
    120|CM\n\
    156|CN\n\
    170|CO\n\
    188|CR\n\
    891|RS ME\n\
    192|CU\n\
    132|CV\n\
    531|CW\n\
    162|CX\n\
    196|CY\n\
    203|CZ\n\
    278|DE\n\
    276|DE\n\
    262|DJ\n\
    208|DK\n\
    212|DM\n\
    214|DO\n\
    012|DZ\n\
    218|EC\n\
    233|EE\n\
    818|EG\n\
    732|EH\n\
    232|ER\n\
    724|ES\n\
    231|ET\n\
    246|FI\n\
    242|FJ\n\
    238|FK\n\
    583|FM\n\
    234|FO\n\
    250|FR\n\
    249|FR\n\
    266|GA\n\
    826|GB\n\
    308|GD\n\
    268|GE\n\
    254|GF\n\
    831|GG\n\
    288|GH\n\
    292|GI\n\
    304|GL\n\
    270|GM\n\
    324|GN\n\
    312|GP\n\
    226|GQ\n\
    300|GR\n\
    239|GS\n\
    320|GT\n\
    316|GU\n\
    624|GW\n\
    328|GY\n\
    344|HK\n\
    334|HM\n\
    340|HN\n\
    191|HR\n\
    332|HT\n\
    348|HU\n\
    360|ID\n\
    372|IE\n\
    376|IL\n\
    833|IM\n\
    356|IN\n\
    086|IO\n\
    368|IQ\n\
    364|IR\n\
    352|IS\n\
    380|IT\n\
    832|JE\n\
    388|JM\n\
    400|JO\n\
    392|JP\n\
    404|KE\n\
    417|KG\n\
    116|KH\n\
    296|KI\n\
    174|KM\n\
    659|KN\n\
    408|KP\n\
    410|KR\n\
    414|KW\n\
    136|KY\n\
    398|KZ\n\
    418|LA\n\
    422|LB\n\
    662|LC\n\
    438|LI\n\
    144|LK\n\
    430|LR\n\
    426|LS\n\
    440|LT\n\
    442|LU\n\
    428|LV\n\
    434|LY\n\
    504|MA\n\
    492|MC\n\
    498|MD\n\
    499|ME\n\
    663|MF\n\
    450|MG\n\
    584|MH\n\
    807|MK\n\
    466|ML\n\
    496|MN\n\
    446|MO\n\
    580|MP\n\
    474|MQ\n\
    478|MR\n\
    500|MS\n\
    470|MT\n\
    480|MU\n\
    462|MV\n\
    454|MW\n\
    484|MX\n\
    458|MY\n\
    508|MZ\n\
    516|NA\n\
    540|NC\n\
    562|NE\n\
    574|NF\n\
    566|NG\n\
    558|NI\n\
    528|NL\n\
    578|NO\n\
    524|NP\n\
    520|NR\n\
    536|SA IQ\n\
    570|NU\n\
    554|NZ\n\
    512|OM\n\
    591|PA\n\
    604|PE\n\
    258|PF\n\
    598|PG\n\
    608|PH\n\
    586|PK\n\
    616|PL\n\
    666|PM\n\
    612|PN\n\
    630|PR\n\
    275|PS\n\
    620|PT\n\
    585|PW\n\
    600|PY\n\
    634|QA\n\
    959|QM\n\
    960|QN\n\
    962|QP\n\
    963|QQ\n\
    964|QR\n\
    965|QS\n\
    966|QT\n\
    967|EU\n\
    968|QV\n\
    969|QW\n\
    970|QX\n\
    971|QY\n\
    972|QZ\n\
    638|RE\n\
    642|RO\n\
    688|RS\n\
    643|RU\n\
    646|RW\n\
    682|SA\n\
    090|SB\n\
    690|SC\n\
    729|SD\n\
    752|SE\n\
    702|SG\n\
    654|SH\n\
    705|SI\n\
    744|SJ\n\
    703|SK\n\
    694|SL\n\
    674|SM\n\
    686|SN\n\
    706|SO\n\
    740|SR\n\
    728|SS\n\
    678|ST\n\
    810|RU AM AZ BY EE GE KZ KG LV LT MD TJ TM UA UZ\n\
    222|SV\n\
    534|SX\n\
    760|SY\n\
    748|SZ\n\
    796|TC\n\
    148|TD\n\
    260|TF\n\
    768|TG\n\
    764|TH\n\
    762|TJ\n\
    772|TK\n\
    626|TL\n\
    795|TM\n\
    788|TN\n\
    776|TO\n\
    792|TR\n\
    780|TT\n\
    798|TV\n\
    158|TW\n\
    834|TZ\n\
    804|UA\n\
    800|UG\n\
    581|UM\n\
    840|US\n\
    858|UY\n\
    860|UZ\n\
    336|VA\n\
    670|VC\n\
    862|VE\n\
    092|VG\n\
    850|VI\n\
    704|VN\n\
    548|VU\n\
    876|WF\n\
    882|WS\n\
    973|XA\n\
    974|XB\n\
    975|XC\n\
    976|XD\n\
    977|XE\n\
    978|XF\n\
    979|XG\n\
    980|XH\n\
    981|XI\n\
    982|XJ\n\
    983|XK\n\
    984|XL\n\
    985|XM\n\
    986|XN\n\
    987|XO\n\
    988|XP\n\
    989|XQ\n\
    990|XR\n\
    991|XS\n\
    992|XT\n\
    993|XU\n\
    994|XV\n\
    995|XW\n\
    996|XX\n\
    997|XY\n\
    998|XZ\n\
    720|YE\n\
    887|YE\n\
    175|YT\n\
    710|ZA\n\
    894|ZM\n\
    716|ZW\n\
    999|ZZ\n\
    ";

/// `<variantAlias type="…" replacement="…"/>`, `type|replacement`.
pub(crate) static VARIANT_ALIAS: &str = "\
    polytoni|polyton\n\
    heploc|alalc97\n\
    ";

/// `<subdivisionAlias type="…" replacement="…"/>`, `type|replacement`,
/// reduced to the first replacement — the only one `-u-sd-`/`-u-rg-`
/// canonicalization can use. An UPPERCASE replacement is a region, not
/// a subdivision (see `cldr_alias.rs`).
pub(crate) static SUBDIVISION_ALIAS: &str = "\
    fi01|AX\n\
    frcp|CP\n\
    shta|TA\n\
    frbl|BL\n\
    frmf|MF\n\
    frnc|NC\n\
    frpf|PF\n\
    frpm|PM\n\
    frtf|TF\n\
    frwf|WF\n\
    nlaw|AW\n\
    nlcw|CW\n\
    nlsx|SX\n\
    usas|AS\n\
    usgu|GU\n\
    usmp|MP\n\
    uspr|PR\n\
    usum|UM\n\
    usvi|VI\n\
    cn11|cnbj\n\
    cn12|cntj\n\
    cn13|cnhe\n\
    cn14|cnsx\n\
    cn15|cnmn\n\
    cn21|cnln\n\
    cn22|cnjl\n\
    cn23|cnhl\n\
    cn31|cnsh\n\
    cn32|cnjs\n\
    cn33|cnzj\n\
    cn34|cnah\n\
    cn35|cnfj\n\
    cn36|cnjx\n\
    cn37|cnsd\n\
    cn41|cnha\n\
    cn42|cnhb\n\
    cn43|cnhn\n\
    cn44|cngd\n\
    cn45|cngx\n\
    cn46|cnhi\n\
    cn50|cncq\n\
    cn51|cnsc\n\
    cn52|cngz\n\
    cn53|cnyn\n\
    cn54|cnxz\n\
    cn61|cnsn\n\
    cn62|cngs\n\
    cn63|cnqh\n\
    cn64|cnnx\n\
    cn65|cnxj\n\
    cn71|TW\n\
    cn91|HK\n\
    cn92|MO\n\
    cz10a|cz110\n\
    cz10b|cz111\n\
    cz10c|cz112\n\
    cz10d|cz113\n\
    cz10e|cz114\n\
    cz10f|cz115\n\
    cz611|cz663\n\
    cz612|cz632\n\
    cz613|cz633\n\
    cz614|cz634\n\
    cz615|cz635\n\
    cz621|cz641\n\
    cz622|cz642\n\
    cz623|cz643\n\
    cz624|cz644\n\
    cz626|cz646\n\
    cz627|cz647\n\
    czjc|cz31\n\
    czjm|cz64\n\
    czka|cz41\n\
    czkr|cz52\n\
    czli|cz51\n\
    czmo|cz80\n\
    czol|cz71\n\
    czpa|cz53\n\
    czpl|cz32\n\
    czpr|cz10\n\
    czst|cz20\n\
    czus|cz42\n\
    czvy|cz63\n\
    czzl|cz72\n\
    fra|frges\n\
    frb|frnaq\n\
    frc|frara\n\
    frd|frbfc\n\
    fre|frbre\n\
    frf|frcvl\n\
    frg|frges\n\
    frgf|GF\n\
    frgp|GP\n\
    frgua|GP\n\
    frh|frcor\n\
    fri|frbfc\n\
    frj|fridf\n\
    frk|frocc\n\
    frl|frnaq\n\
    frlre|RE\n\
    frm|frges\n\
    frmay|YT\n\
    frmq|MQ\n\
    frn|frocc\n\
    fro|frhdf\n\
    frp|frnor\n\
    frq|frnor\n\
    frr|frpdl\n\
    frre|RE\n\
    frs|frhdf\n\
    frt|frnaq\n\
    fru|frpac\n\
    frv|frara\n\
    fryt|YT\n\
    laxn|laxs\n\
    lud|lucl\n\
    lug|luec\n\
    lul|luca\n\
    mrnkc|mr13\n\
    no23|no50\n\
    nzn|nzauk\n\
    nzs|nzcan\n\
    omba|ombj\n\
    omsh|omsj\n\
    plds|pl02\n\
    plkp|pl04\n\
    pllb|pl08\n\
    plld|pl10\n\
    pllu|pl06\n\
    plma|pl12\n\
    plmz|pl14\n\
    plop|pl16\n\
    plpd|pl20\n\
    plpk|pl18\n\
    plpm|pl22\n\
    plsk|pl26\n\
    plsl|pl24\n\
    plwn|pl28\n\
    plwp|pl30\n\
    plzp|pl32\n\
    tteto|tttob\n\
    ttrcm|ttmrc\n\
    ttwto|tttob\n\
    twkhq|twkhh\n\
    twtnq|twtnn\n\
    twtpq|twnwt\n\
    twtxq|twtxg\n\
    ";

/// `-u-`/`-t-` keyword type aliases from `common/bcp47/*.xml`:
/// `key|alias|canonical`. Covers both `<type name=N alias="A"/>` and
/// `<type name=N deprecated="true" preferred=P/>`; spellings that
/// cannot occur in an extension (not a `uvalue`) are omitted.
pub(crate) static BCP47_TYPE_ALIAS: &str = "\
    ca|ethiopic-amete-alem|ethioaa\n\
    ca|islamicc|islamic-civil\n\
    d0|name|charname\n\
    kb|yes|true\n\
    kc|yes|true\n\
    kh|yes|true\n\
    kk|yes|true\n\
    kn|yes|true\n\
    ks|primary|level1\n\
    ks|tertiary|level3\n\
    m0|beta-metsehaf|betamets\n\
    m0|ies-jes|iesjes\n\
    m0|names|prprname\n\
    m0|tekie-alibekit|tekieali\n\
    ms|imperial|uksystem\n\
    tz|aqams|nzakl\n\
    tz|aukns|auhba\n\
    tz|caffs|cawnp\n\
    tz|camtr|cator\n\
    tz|canpg|cator\n\
    tz|capnt|caiql\n\
    tz|cathu|cator\n\
    tz|cayzf|caedm\n\
    tz|cet|bebru\n\
    tz|cnckg|cnsha\n\
    tz|cnhrb|cnsha\n\
    tz|cnkhg|cnurc\n\
    tz|cst6cdt|uschi\n\
    tz|cuba|cuhav\n\
    tz|eet|grath\n\
    tz|egypt|egcai\n\
    tz|eire|iedub\n\
    tz|est|papty\n\
    tz|est5edt|usnyc\n\
    tz|factory|unk\n\
    tz|gaza|gazastrp\n\
    tz|gmt0|gmt\n\
    tz|hongkong|hkhkg\n\
    tz|hst|ushnl\n\
    tz|iceland|isrey\n\
    tz|iran|irthr\n\
    tz|israel|jeruslm\n\
    tz|jamaica|jmkin\n\
    tz|japan|jptyo\n\
    tz|libya|lytip\n\
    tz|met|bebru\n\
    tz|mncoq|mnuln\n\
    tz|mst|usphx\n\
    tz|mst7mdt|usden\n\
    tz|mxstis|mxtij\n\
    tz|navajo|usden\n\
    tz|poland|plwaw\n\
    tz|portugal|ptlis\n\
    tz|prc|cnsha\n\
    tz|pst8pdt|uslax\n\
    tz|roc|twtpe\n\
    tz|rok|krsel\n\
    tz|turkey|trist\n\
    tz|uaozh|uaiev\n\
    tz|uauzh|uaiev\n\
    tz|uct|utc\n\
    tz|umjon|ushnl\n\
    tz|usnavajo|usden\n\
    tz|wet|ptlis\n\
    tz|zulu|utc\n\
    ";

/// `<likelySubtag from="und" to="en_Latn_US"/>` — the root of §4.3.
pub(crate) static LIKELY_UND: (&str, &str, &str) = ("en", "Latn", "US");

/// `<likelySubtag from="L" to="L_S_R"/>` as `L S R`. The target
/// language always repeats the source, so it is not stored twice.
pub(crate) static LIKELY_LANG: &str = "\
    aa Latn ET\n\
    ab Cyrl GE\n\
    abr Latn GH\n\
    ace Latn ID\n\
    ach Latn UG\n\
    ada Latn GH\n\
    ady Cyrl RU\n\
    ae Avst IR\n\
    aeb Arab TN\n\
    af Latn ZA\n\
    agq Latn CM\n\
    aho Ahom IN\n\
    ak Latn GH\n\
    akk Xsux IQ\n\
    aln Latn XK\n\
    alt Cyrl RU\n\
    am Ethi ET\n\
    amo Latn NG\n\
    an Latn ES\n\
    ann Latn NG\n\
    aoz Latn ID\n\
    apc Arab SY\n\
    apd Arab SD\n\
    ar Arab EG\n\
    arc Armi IR\n\
    arn Latn CL\n\
    aro Latn BO\n\
    arq Arab DZ\n\
    ars Arab SA\n\
    ary Arab MA\n\
    arz Arab EG\n\
    as Beng IN\n\
    asa Latn TZ\n\
    ase Sgnw US\n\
    ast Latn ES\n\
    atj Latn CA\n\
    av Cyrl RU\n\
    awa Deva IN\n\
    ay Latn BO\n\
    az Latn AZ\n\
    ba Cyrl RU\n\
    bal Arab PK\n\
    ban Latn ID\n\
    bap Deva NP\n\
    bar Latn AT\n\
    bas Latn CM\n\
    bax Bamu CM\n\
    bbc Latn ID\n\
    bbj Latn CM\n\
    bci Latn CI\n\
    be Cyrl BY\n\
    bej Arab SD\n\
    bem Latn ZM\n\
    bew Latn ID\n\
    bez Latn TZ\n\
    bfd Latn CM\n\
    bfq Taml IN\n\
    bft Arab PK\n\
    bfy Deva IN\n\
    bg Cyrl BG\n\
    bgc Deva IN\n\
    bgn Arab PK\n\
    bgx Grek TR\n\
    bhb Deva IN\n\
    bhi Deva IN\n\
    bho Deva IN\n\
    bi Latn VU\n\
    bik Latn PH\n\
    bin Latn NG\n\
    bjj Deva IN\n\
    bjn Latn ID\n\
    bjt Latn SN\n\
    bkm Latn CM\n\
    bku Latn PH\n\
    bla Latn CA\n\
    blo Latn BJ\n\
    blt Tavt VN\n\
    bm Latn ML\n\
    bmq Latn ML\n\
    bn Beng BD\n\
    bo Tibt CN\n\
    bpy Beng IN\n\
    bqi Arab IR\n\
    bqv Latn CI\n\
    br Latn FR\n\
    bra Deva IN\n\
    brh Arab PK\n\
    brx Deva IN\n\
    bs Latn BA\n\
    bsc Latn SN\n\
    bsq Bass LR\n\
    bss Latn CM\n\
    bto Latn PH\n\
    btv Deva PK\n\
    bua Cyrl RU\n\
    buc Latn YT\n\
    bug Latn ID\n\
    bum Latn CM\n\
    bvb Latn GQ\n\
    byn Ethi ER\n\
    byv Latn CM\n\
    bze Latn ML\n\
    ca Latn ES\n\
    cad Latn US\n\
    cch Latn NG\n\
    ccp Cakm BD\n\
    ccr Latn SV\n\
    ce Cyrl RU\n\
    ceb Latn PH\n\
    cgg Latn UG\n\
    ch Latn GU\n\
    chk Latn FM\n\
    chm Cyrl RU\n\
    cho Latn US\n\
    chp Latn CA\n\
    chr Cher US\n\
    cic Latn US\n\
    cja Arab KH\n\
    cjm Cham VN\n\
    ckb Arab IQ\n\
    clc Latn CA\n\
    cmg Soyo MN\n\
    co Latn FR\n\
    cop Copt EG\n\
    cps Latn PH\n\
    cr Cans CA\n\
    crg Latn CA\n\
    crh Cyrl UA\n\
    crk Cans CA\n\
    crl Cans CA\n\
    crs Latn SC\n\
    cs Latn CZ\n\
    csb Latn PL\n\
    csw Cans CA\n\
    ctd Pauc MM\n\
    cu Cyrl RU\n\
    cv Cyrl RU\n\
    cy Latn GB\n\
    da Latn DK\n\
    dak Latn US\n\
    dar Cyrl RU\n\
    dav Latn KE\n\
    dcc Arab IN\n\
    de Latn DE\n\
    den Latn CA\n\
    dgr Latn CA\n\
    dje Latn NE\n\
    dmf Medf NG\n\
    dnj Latn CI\n\
    doi Deva IN\n\
    dsb Latn DE\n\
    dtm Latn ML\n\
    dtp Latn MY\n\
    dty Deva NP\n\
    dua Latn CM\n\
    dv Thaa MV\n\
    dyo Latn SN\n\
    dyu Latn BF\n\
    dz Tibt BT\n\
    ebu Latn KE\n\
    ecy Cprt CY\n\
    ee Latn GH\n\
    efi Latn NG\n\
    egl Latn IT\n\
    egy Egyp EG\n\
    eky Kali MM\n\
    el Grek GR\n\
    en Latn US\n\
    eo Latn 001\n\
    es Latn ES\n\
    esg Gonm IN\n\
    esu Latn US\n\
    et Latn EE\n\
    ett Ital IT\n\
    eu Latn ES\n\
    ewo Latn CM\n\
    ext Latn ES\n\
    fa Arab IR\n\
    fan Latn GQ\n\
    fbl Latn PH\n\
    ff Latn SN\n\
    ffm Latn ML\n\
    fi Latn FI\n\
    fia Arab SD\n\
    fil Latn PH\n\
    fit Latn SE\n\
    fj Latn FJ\n\
    fo Latn FO\n\
    fon Latn BJ\n\
    fr Latn FR\n\
    frc Latn US\n\
    frp Latn FR\n\
    frr Latn DE\n\
    frs Latn DE\n\
    fub Arab CM\n\
    fud Latn WF\n\
    fuf Latn GN\n\
    fuq Latn NE\n\
    fur Latn IT\n\
    fuv Latn NG\n\
    fvr Latn SD\n\
    fy Latn NL\n\
    ga Latn IE\n\
    gaa Latn GH\n\
    gag Latn MD\n\
    gan Hans CN\n\
    gay Latn ID\n\
    gbm Deva IN\n\
    gbz Arab IR\n\
    gcr Latn GF\n\
    gd Latn GB\n\
    gez Ethi ET\n\
    gil Latn KI\n\
    gjk Arab PK\n\
    gju Arab PK\n\
    gl Latn ES\n\
    glk Arab IR\n\
    gmy Linb GR\n\
    gn Latn PY\n\
    gon Deva IN\n\
    gor Latn ID\n\
    gos Latn NL\n\
    got Goth UA\n\
    grc Grek GR\n\
    grt Beng IN\n\
    gsw Latn CH\n\
    gu Gujr IN\n\
    gub Latn BR\n\
    guc Latn CO\n\
    gur Latn GH\n\
    guz Latn KE\n\
    gv Latn IM\n\
    gvr Deva NP\n\
    gwi Latn CA\n\
    ha Latn NG\n\
    hak Hans CN\n\
    haw Latn US\n\
    haz Arab AF\n\
    he Hebr IL\n\
    hi Deva IN\n\
    hif Deva FJ\n\
    hil Latn PH\n\
    hlu Hluw TR\n\
    hmd Plrd CN\n\
    hnd Arab PK\n\
    hne Deva IN\n\
    hnj Hmnp US\n\
    hnn Latn PH\n\
    hno Arab PK\n\
    ho Latn PG\n\
    hoc Deva IN\n\
    hoj Deva IN\n\
    hr Latn HR\n\
    hsb Latn DE\n\
    hsn Hans CN\n\
    ht Latn HT\n\
    hu Latn HU\n\
    hur Latn CA\n\
    hy Armn AM\n\
    hz Latn NA\n\
    ia Latn 001\n\
    iba Latn MY\n\
    ibb Latn NG\n\
    id Latn ID\n\
    ie Latn EE\n\
    ife Latn TG\n\
    ig Latn NG\n\
    ii Yiii CN\n\
    ik Latn US\n\
    ilo Latn PH\n\
    in Latn ID\n\
    inh Cyrl RU\n\
    io Latn 001\n\
    is Latn IS\n\
    it Latn IT\n\
    iu Cans CA\n\
    iw Hebr IL\n\
    izh Latn RU\n\
    ja Jpan JP\n\
    jam Latn JM\n\
    jbo Latn 001\n\
    jgo Latn CM\n\
    ji Hebr UA\n\
    jmc Latn TZ\n\
    jml Deva NP\n\
    jut Latn DK\n\
    jv Latn ID\n\
    jw Latn ID\n\
    ka Geor GE\n\
    kaa Cyrl UZ\n\
    kab Latn DZ\n\
    kac Latn MM\n\
    kaj Latn NG\n\
    kam Latn KE\n\
    kao Latn ML\n\
    kaw Bali ID\n\
    kbd Cyrl RU\n\
    kby Arab NE\n\
    kcg Latn NG\n\
    kck Latn ZW\n\
    kde Latn TZ\n\
    kdh Latn TG\n\
    kdt Thai TH\n\
    kea Latn CV\n\
    ken Latn CM\n\
    kfo Latn CI\n\
    kfr Deva IN\n\
    kfy Deva IN\n\
    kg Latn CD\n\
    kge Latn ID\n\
    kgp Latn BR\n\
    kha Latn IN\n\
    khb Talu CN\n\
    khn Deva IN\n\
    khq Latn ML\n\
    kht Mymr IN\n\
    khw Arab PK\n\
    ki Latn KE\n\
    kiu Latn TR\n\
    kj Latn NA\n\
    kjg Laoo LA\n\
    kk Cyrl KZ\n\
    kkj Latn CM\n\
    kl Latn GL\n\
    kln Latn KE\n\
    km Khmr KH\n\
    kmb Latn AO\n\
    kn Knda IN\n\
    knf Latn GW\n\
    knn Deva IN\n\
    ko Kore KR\n\
    koi Cyrl RU\n\
    kok Deva IN\n\
    kos Latn FM\n\
    kpe Latn LR\n\
    kqn Latn ZM\n\
    krc Cyrl RU\n\
    kri Latn SL\n\
    krj Latn PH\n\
    krl Latn RU\n\
    kru Deva IN\n\
    ks Arab IN\n\
    ksb Latn TZ\n\
    ksf Latn CM\n\
    ksh Latn DE\n\
    ku Latn TR\n\
    kum Cyrl RU\n\
    kv Cyrl RU\n\
    kvr Latn ID\n\
    kvx Arab PK\n\
    kw Latn GB\n\
    kwk Latn CA\n\
    kxm Thai TH\n\
    kxp Arab PK\n\
    kxv Latn IN\n\
    ky Cyrl KG\n\
    la Latn VA\n\
    lab Lina GR\n\
    lad Hebr IL\n\
    lag Latn TZ\n\
    lah Arab PK\n\
    laj Latn UG\n\
    lb Latn LU\n\
    lbe Cyrl RU\n\
    lbw Latn ID\n\
    lcp Thai CN\n\
    leb Latn ZM\n\
    len Latn SV\n\
    lep Lepc IN\n\
    lez Cyrl RU\n\
    lg Latn UG\n\
    li Latn NL\n\
    lif Deva NP\n\
    lij Latn IT\n\
    lil Latn CA\n\
    lis Lisu CN\n\
    ljp Latn ID\n\
    lki Arab IR\n\
    lkt Latn US\n\
    lld Latn IT\n\
    lmn Telu IN\n\
    lmo Latn IT\n\
    ln Latn CD\n\
    lo Laoo LA\n\
    lol Latn CD\n\
    loz Latn ZM\n\
    lrc Arab IR\n\
    lt Latn LT\n\
    ltg Latn LV\n\
    lu Latn CD\n\
    lua Latn CD\n\
    lue Latn ZM\n\
    lun Latn ZM\n\
    luo Latn KE\n\
    luy Latn KE\n\
    luz Arab IR\n\
    lv Latn LV\n\
    lwl Thai TH\n\
    lzh Hans CN\n\
    lzz Latn TR\n\
    mad Latn ID\n\
    maf Latn CM\n\
    mag Deva IN\n\
    mai Deva IN\n\
    mak Latn ID\n\
    man Latn GM\n\
    mas Latn KE\n\
    maz Latn MX\n\
    mdf Cyrl RU\n\
    mdh Latn PH\n\
    mdr Latn ID\n\
    men Latn SL\n\
    mer Latn KE\n\
    mey Latn SN\n\
    mfa Arab TH\n\
    mfe Latn MU\n\
    mfv Latn SN\n\
    mg Latn MG\n\
    mgh Latn MZ\n\
    mgo Latn CM\n\
    mgp Deva NP\n\
    mgy Latn TZ\n\
    mh Latn MH\n\
    mhn Latn IT\n\
    mi Latn NZ\n\
    mic Latn CA\n\
    min Latn ID\n\
    mk Cyrl MK\n\
    ml Mlym IN\n\
    mls Latn SD\n\
    mn Cyrl MN\n\
    mni Beng IN\n\
    mnw Mymr MM\n\
    mo Latn RO\n\
    moe Latn CA\n\
    moh Latn CA\n\
    mos Latn BF\n\
    mr Deva IN\n\
    mrd Deva NP\n\
    mrj Cyrl RU\n\
    mro Mroo BD\n\
    ms Latn MY\n\
    mt Latn MT\n\
    mtr Deva IN\n\
    mua Latn CM\n\
    mus Latn US\n\
    mvy Arab PK\n\
    mwk Latn ML\n\
    mwr Deva IN\n\
    mwv Latn ID\n\
    mww Hmnp US\n\
    mxc Latn ZW\n\
    my Mymr MM\n\
    myv Cyrl RU\n\
    myx Latn UG\n\
    myz Mand IR\n\
    mzn Arab IR\n\
    na Latn NR\n\
    nan Hans CN\n\
    nap Latn IT\n\
    naq Latn NA\n\
    nb Latn NO\n\
    nch Latn MX\n\
    nd Latn ZW\n\
    ndc Latn MZ\n\
    nds Latn DE\n\
    ne Deva NP\n\
    new Deva NP\n\
    ng Latn NA\n\
    ngl Latn MZ\n\
    nhe Latn MX\n\
    nhw Latn MX\n\
    nij Latn ID\n\
    niu Latn NU\n\
    njo Latn IN\n\
    nl Latn NL\n\
    nmg Latn CM\n\
    nn Latn NO\n\
    nnh Latn CM\n\
    nnp Wcho IN\n\
    no Latn NO\n\
    nod Lana TH\n\
    noe Deva IN\n\
    non Runr SE\n\
    nqo Nkoo GN\n\
    nr Latn ZA\n\
    nse Latn ZM\n\
    nsk Cans CA\n\
    nso Latn ZA\n\
    nst Tnsa IN\n\
    nus Latn SS\n\
    nv Latn US\n\
    nxq Latn CN\n\
    ny Latn MW\n\
    nym Latn TZ\n\
    nyn Latn UG\n\
    nzi Latn GH\n\
    oc Latn FR\n\
    oj Cans CA\n\
    ojs Cans CA\n\
    ojw Latn CA\n\
    oka Latn CA\n\
    om Latn ET\n\
    or Orya IN\n\
    os Cyrl GE\n\
    osa Osge US\n\
    otk Orkh MN\n\
    oui Ougr CN\n\
    pa Guru IN\n\
    pag Latn PH\n\
    pal Phli IR\n\
    pam Latn PH\n\
    pap Latn CW\n\
    pau Latn PW\n\
    pcd Latn FR\n\
    pcm Latn NG\n\
    pdc Latn US\n\
    pdt Latn CA\n\
    peo Xpeo IR\n\
    pfl Latn DE\n\
    phn Phnx LB\n\
    pis Latn SB\n\
    pka Brah IN\n\
    pko Latn KE\n\
    pl Latn PL\n\
    pms Latn IT\n\
    pnt Grek GR\n\
    pon Latn FM\n\
    ppl Latn SV\n\
    pqm Latn CA\n\
    pra Khar PK\n\
    prd Arab IR\n\
    prg Latn PL\n\
    ps Arab AF\n\
    pt Latn BR\n\
    puu Latn GA\n\
    qu Latn PE\n\
    quc Latn GT\n\
    qug Latn EC\n\
    raj Deva IN\n\
    rcf Latn RE\n\
    rej Latn ID\n\
    rgn Latn IT\n\
    rhg Rohg MM\n\
    ria Latn IN\n\
    rif Latn MA\n\
    rjs Deva NP\n\
    rkt Beng BD\n\
    rm Latn CH\n\
    rmf Latn FI\n\
    rmo Latn CH\n\
    rmt Arab IR\n\
    rmu Latn SE\n\
    rn Latn BI\n\
    rng Latn MZ\n\
    ro Latn RO\n\
    rob Latn ID\n\
    rof Latn TZ\n\
    rtm Latn FJ\n\
    ru Cyrl RU\n\
    rue Cyrl UA\n\
    rug Latn SB\n\
    rw Latn RW\n\
    rwk Latn TZ\n\
    ryu Kana JP\n\
    sa Deva IN\n\
    saf Latn GH\n\
    sah Cyrl RU\n\
    saq Latn KE\n\
    sas Latn ID\n\
    sat Olck IN\n\
    sav Latn SN\n\
    saz Saur IN\n\
    sbp Latn TZ\n\
    sc Latn IT\n\
    sck Deva IN\n\
    scn Latn IT\n\
    sco Latn GB\n\
    sd Arab PK\n\
    sdc Latn IT\n\
    sdh Arab IR\n\
    se Latn NO\n\
    sef Latn CI\n\
    seh Latn MZ\n\
    sei Latn MX\n\
    ses Latn ML\n\
    sg Latn CF\n\
    sga Latn IE\n\
    sgs Latn LT\n\
    shi Tfng MA\n\
    shn Mymr MM\n\
    si Sinh LK\n\
    sid Latn ET\n\
    sk Latn SK\n\
    skr Arab PK\n\
    sl Latn SI\n\
    sli Latn PL\n\
    sly Latn ID\n\
    sm Latn WS\n\
    sma Latn SE\n\
    smj Latn SE\n\
    smn Latn FI\n\
    smp Samr IL\n\
    sms Latn FI\n\
    sn Latn ZW\n\
    snf Latn SN\n\
    snk Latn ML\n\
    so Latn SO\n\
    sog Sogd UZ\n\
    sou Thai TH\n\
    sq Latn AL\n\
    sr Cyrl RS\n\
    srb Sora IN\n\
    srn Latn SR\n\
    srr Latn SN\n\
    srx Deva IN\n\
    ss Latn ZA\n\
    ssy Latn ER\n\
    st Latn ZA\n\
    stq Latn DE\n\
    su Latn ID\n\
    suk Latn TZ\n\
    sus Latn GN\n\
    suz Sunu NP\n\
    sv Latn SE\n\
    sw Latn TZ\n\
    swb Arab YT\n\
    swg Latn DE\n\
    swv Deva IN\n\
    sxn Latn ID\n\
    syl Beng BD\n\
    syr Syrc IQ\n\
    szl Latn PL\n\
    ta Taml IN\n\
    taj Deva NP\n\
    tbw Latn PH\n\
    tcy Knda IN\n\
    tdd Tale CN\n\
    tdg Deva NP\n\
    tdh Deva NP\n\
    te Telu IN\n\
    tem Latn SL\n\
    teo Latn UG\n\
    tet Latn TL\n\
    tg Cyrl TJ\n\
    th Thai TH\n\
    thl Deva NP\n\
    thq Deva NP\n\
    thr Deva NP\n\
    ti Ethi ET\n\
    tig Ethi ER\n\
    tiv Latn NG\n\
    tk Latn TM\n\
    tkl Latn TK\n\
    tkr Latn AZ\n\
    tkt Deva NP\n\
    tl Latn PH\n\
    tly Latn AZ\n\
    tmh Latn NE\n\
    tn Latn ZA\n\
    tnr Latn SN\n\
    to Latn TO\n\
    tog Latn MW\n\
    toi Latn ZM\n\
    tok Latn 001\n\
    tpi Latn PG\n\
    tr Latn TR\n\
    tru Latn TR\n\
    trv Latn TW\n\
    trw Arab PK\n\
    ts Latn ZA\n\
    tsd Grek GR\n\
    tsg Latn PH\n\
    tsj Tibt BT\n\
    tt Cyrl RU\n\
    ttj Latn UG\n\
    tts Thai TH\n\
    ttt Latn AZ\n\
    tum Latn MW\n\
    tvl Latn TV\n\
    twq Latn NE\n\
    txg Tang CN\n\
    txo Toto IN\n\
    ty Latn PF\n\
    tyv Cyrl RU\n\
    tzm Latn MA\n\
    udm Cyrl RU\n\
    ug Arab CN\n\
    uga Ugar SY\n\
    uk Cyrl UA\n\
    uli Latn FM\n\
    umb Latn AO\n\
    unr Beng IN\n\
    unx Beng IN\n\
    ur Arab PK\n\
    uz Latn UZ\n\
    vai Vaii LR\n\
    ve Latn ZA\n\
    vec Latn IT\n\
    vep Latn RU\n\
    vi Latn VN\n\
    vic Latn SX\n\
    vls Latn BE\n\
    vmf Latn DE\n\
    vmw Latn MZ\n\
    vo Latn 001\n\
    vot Latn RU\n\
    vro Latn EE\n\
    vun Latn TZ\n\
    wa Latn BE\n\
    wae Latn CH\n\
    wal Ethi ET\n\
    war Latn PH\n\
    wbp Latn AU\n\
    wbq Telu IN\n\
    wbr Deva IN\n\
    wls Latn WF\n\
    wni Arab KM\n\
    wo Latn SN\n\
    wsg Gong IN\n\
    wtm Deva IN\n\
    wuu Hans CN\n\
    xag Aghb AZ\n\
    xav Latn BR\n\
    xco Chrs UZ\n\
    xcr Cari TR\n\
    xh Latn ZA\n\
    xlc Lyci TR\n\
    xld Lydi TR\n\
    xmf Geor GE\n\
    xmn Mani CN\n\
    xmr Merc SD\n\
    xna Narb SA\n\
    xnr Deva IN\n\
    xog Latn UG\n\
    xpr Prti IR\n\
    xsa Sarb YE\n\
    xsr Deva NP\n\
    yao Latn MZ\n\
    yap Latn FM\n\
    yav Latn CM\n\
    ybb Latn CM\n\
    yi Hebr UA\n\
    yo Latn NG\n\
    yrl Latn BR\n\
    yua Latn MX\n\
    yue Hant HK\n\
    za Latn CN\n\
    zag Latn SD\n\
    zdj Arab KM\n\
    zea Latn NL\n\
    zgh Tfng MA\n\
    zh Hans CN\n\
    zhx Nshu CN\n\
    zkt Kits CN\n\
    zlm Latn MY\n\
    zmi Latn MY\n\
    zu Latn ZA\n\
    zza Latn TR\n\
    aaa Latn NG\n\
    aab Latn NG\n\
    aac Latn PG\n\
    aad Latn PG\n\
    aae Latn IT\n\
    aaf Mlym IN\n\
    aag Latn PG\n\
    aah Latn PG\n\
    aai Latn PG\n\
    aak Latn PG\n\
    aal Latn CM\n\
    aan Latn BR\n\
    aao Arab DZ\n\
    aap Latn BR\n\
    aaq Latn US\n\
    aas Latn TZ\n\
    aat Grek GR\n\
    aau Latn PG\n\
    aaw Latn PG\n\
    aax Latn ID\n\
    aaz Latn ID\n\
    aba Latn CI\n\
    abb Latn CM\n\
    abc Latn PH\n\
    abd Latn PH\n\
    abe Latn CA\n\
    abf Latn MY\n\
    abg Latn PG\n\
    abh Arab TJ\n\
    abi Latn CI\n\
    abl Rjng ID\n\
    abm Latn NG\n\
    abn Latn NG\n\
    abo Latn NG\n\
    abp Latn PH\n\
    abs Latn ID\n\
    abt Latn PG\n\
    abu Latn CI\n\
    abv Arab BH\n\
    abw Latn PG\n\
    abx Latn PH\n\
    aby Latn PG\n\
    abz Latn ID\n\
    aca Latn CO\n\
    acb Latn NG\n\
    acd Latn GH\n\
    acf Latn LC\n\
    acm Arab IQ\n\
    acn Latn CN\n\
    acp Latn NG\n\
    acq Arab YE\n\
    acr Latn GT\n\
    acs Latn BR\n\
    act Latn NL\n\
    acu Latn EC\n\
    acv Latn US\n\
    acw Arab SA\n\
    acx Arab OM\n\
    acy Latn CY\n\
    acz Latn SD\n\
    adb Latn TL\n\
    add Latn CM\n\
    ade Latn TG\n\
    adf Arab OM\n\
    adg Latn AU\n\
    adh Latn UG\n\
    adi Latn IN\n\
    adj Latn CI\n\
    adl Latn IN\n\
    adn Latn ID\n\
    ado Latn PG\n\
    adq Latn GH\n\
    adr Latn ID\n\
    adt Latn AU\n\
    adu Latn NG\n\
    adw Latn BR\n\
    adx Tibt CN\n\
    adz Latn PG\n\
    aea Latn AU\n\
    aec Arab EG\n\
    aee Arab AF\n\
    aek Latn NC\n\
    ael Latn CM\n\
    aem Latn VN\n\
    aeq Arab PK\n\
    aer Latn AU\n\
    aeu Latn CN\n\
    aew Latn PG\n\
    aey Latn PG\n\
    aez Latn PG\n\
    afb Arab KW\n\
    afd Latn PG\n\
    afe Latn NG\n\
    afh Latn GH\n\
    afi Latn PG\n\
    afk Latn PG\n\
    afn Latn NG\n\
    afo Latn NG\n\
    afp Latn PG\n\
    afs Latn MX\n\
    afu Latn GH\n\
    afz Latn ID\n\
    aga Latn PE\n\
    agb Latn NG\n\
    agc Latn NG\n\
    agd Latn PG\n\
    age Latn PG\n\
    agf Latn ID\n\
    agg Latn PG\n\
    agh Latn CD\n\
    agi Deva IN\n\
    agj Ethi ET\n\
    agk Latn PH\n\
    agl Latn PG\n\
    agm Latn PG\n\
    agn Latn PH\n\
    ago Latn PG\n\
    agr Latn PE\n\
    ags Latn CM\n\
    agt Latn PH\n\
    agu Latn GT\n\
    agv Latn PH\n\
    agw Latn SB\n\
    agx Cyrl RU\n\
    agy Latn PH\n\
    agz Latn PH\n\
    aha Latn GH\n\
    ahb Latn VU\n\
    ahg Ethi ET\n\
    ahh Latn ID\n\
    ahi Latn CI\n\
    ahk Latn MM\n\
    ahl Latn TG\n\
    ahm Latn CI\n\
    ahn Latn NG\n\
    ahp Latn CI\n\
    ahr Deva IN\n\
    ahs Latn NG\n\
    aht Latn US\n\
    aia Latn SB\n\
    aib Arab CN\n\
    aic Latn PG\n\
    aid Latn AU\n\
    aie Latn PG\n\
    aif Latn PG\n\
    aig Latn AG\n\
    aii Syrc IQ\n\
    aij Hebr IL\n\
    aik Latn NG\n\
    ail Latn PG\n\
    aim Latn IN\n\
    ain Kana JP\n\
    aio Mymr IN\n\
    aip Latn ID\n\
    aiq Arab AF\n\
    air Latn ID\n\
    ait Latn BR\n\
    aiw Latn ET\n\
    aix Latn PG\n\
    aiy Latn CF\n\
    aja Latn SS\n\
    ajg Latn BJ\n\
    aji Latn NC\n\
    ajn Latn AU\n\
    ajw Latn NG\n\
    ajz Latn IN\n\
    akb Latn ID\n\
    akc Latn ID\n\
    akd Latn NG\n\
    ake Latn GY\n\
    akf Latn NG\n\
    akg Latn ID\n\
    akh Latn PG\n\
    aki Latn PG\n\
    akl Latn PH\n\
    ako Latn SR\n\
    akp Latn GH\n\
    akq Latn PG\n\
    akr Latn VU\n\
    aks Latn TG\n\
    akt Latn PG\n\
    aku Latn CM\n\
    akv Cyrl RU\n\
    akw Latn CG\n\
    akz Latn US\n\
    ala Latn NG\n\
    alc Latn CL\n\
    ald Latn CI\n\
    ale Latn US\n\
    alf Latn NG\n\
    alh Latn AU\n\
    ali Latn PG\n\
    alj Latn PH\n\
    alk Laoo LA\n\
    all Mlym IN\n\
    alm Latn VU\n\
    alo Latn ID\n\
    alp Latn ID\n\
    alq Latn CA\n\
    alr Cyrl RU\n\
    alu Latn SB\n\
    alw Ethi ET\n\
    alx Latn PG\n\
    aly Latn AU\n\
    alz Latn CD\n\
    ama Latn BR\n\
    amb Latn NG\n\
    amc Latn PE\n\
    ame Latn PE\n\
    amf Latn ET\n\
    amg Latn AU\n\
    ami Latn TW\n\
    amj Latn TD\n\
    amk Latn ID\n\
    amm Latn PG\n\
    amn Latn PG\n\
    amp Latn PG\n\
    amq Latn ID\n\
    amr Latn PE\n\
    ams Jpan JP\n\
    amt Latn PG\n\
    amu Latn MX\n\
    amv Latn ID\n\
    amw Syrc SY\n\
    amx Latn AU\n\
    amy Latn AU\n\
    amz Latn AU\n\
    ana Latn CO\n\
    anb Latn PE\n\
    anc Latn NG\n\
    and Latn ID\n\
    ane Latn NC\n\
    anf Latn GH\n\
    ang Latn GB\n\
    anh Latn PG\n\
    ani Cyrl RU\n\
    anj Latn PG\n\
    ank Latn NG\n\
    anl Latn MM\n\
    anm Latn IN\n\
    ano Latn CO\n\
    anp Deva IN\n\
    anq Deva IN\n\
    anr Deva IN\n\
    ans Latn CO\n\
    ant Latn AU\n\
    anu Ethi ET\n\
    anv Latn CM\n\
    anw Latn NG\n\
    anx Latn PG\n\
    any Latn CI\n\
    anz Latn PG\n\
    aoa Latn ST\n\
    aob Latn PG\n\
    aoc Latn VE\n\
    aod Latn PG\n\
    aoe Latn PG\n\
    aof Latn PG\n\
    aog Latn PG\n\
    aoi Latn AU\n\
    aoj Latn PG\n\
    aok Latn NC\n\
    aol Latn ID\n\
    aom Latn PG\n\
    aon Latn PG\n\
    aor Latn VU\n\
    aos Latn ID\n\
    aot Beng BD\n\
    aox Latn GY\n\
    apb Latn SB\n\
    ape Latn PG\n\
    apf Latn PH\n\
    apg Latn ID\n\
    aph Deva NP\n\
    api Latn BR\n\
    apj Latn US\n\
    apk Latn US\n\
    apl Latn US\n\
    apm Latn US\n\
    apn Latn BR\n\
    apo Latn PG\n\
    app Latn VU\n\
    apr Latn PG\n\
    aps Latn PG\n\
    apt Latn IN\n\
    apu Latn BR\n\
    apv Latn BR\n\
    apw Latn US\n\
    apx Latn ID\n\
    apy Latn BR\n\
    apz Latn PG\n\
    aqc Cyrl RU\n\
    aqd Latn ML\n\
    aqg Latn NG\n\
    aqk Latn NG\n\
    aqm Latn ID\n\
    aqn Latn PH\n\
    aqr Latn NC\n\
    aqt Latn PY\n\
    aqz Latn BR\n\
    ard Latn AU\n\
    are Latn AU\n\
    arh Latn CO\n\
    ari Latn US\n\
    arj Latn BR\n\
    ark Latn BR\n\
    arl Latn PE\n\
    arp Latn US\n\
    arr Latn BR\n\
    aru Latn BR\n\
    arw Latn SR\n\
    arx Latn BR\n\
    asb Latn CA\n\
    asc Latn ID\n\
    asg Latn NG\n\
    ash Latn PE\n\
    asi Latn ID\n\
    asj Latn CM\n\
    ask Arab AF\n\
    asl Latn ID\n\
    asn Latn BR\n\
    aso Latn PG\n\
    asr Deva IN\n\
    ass Latn CM\n\
    asu Latn BR\n\
    asv Latn CD\n\
    asx Latn PG\n\
    asy Latn ID\n\
    asz Latn ID\n\
    ata Latn PG\n\
    atb Latn CN\n\
    atc Latn PE\n\
    atd Latn PH\n\
    ate Latn PG\n\
    atg Latn NG\n\
    ati Latn CI\n\
    atk Latn PH\n\
    atl Latn PH\n\
    atm Latn PH\n\
    atn Arab IR\n\
    ato Latn CM\n\
    atp Latn PH\n\
    atq Latn ID\n\
    atr Latn BR\n\
    ats Latn US\n\
    att Latn PH\n\
    atu Latn SS\n\
    atv Cyrl RU\n\
    atw Latn US\n\
    atx Latn BR\n\
    aty Latn VU\n\
    atz Latn PH\n\
    aua Latn SB\n\
    auc Latn EC\n\
    aud Latn SB\n\
    aug Latn BJ\n\
    auh Latn ZM\n\
    aui Latn PG\n\
    auj Arab LY\n\
    auk Latn PG\n\
    aul Latn VU\n\
    aum Latn NG\n\
    aun Latn PG\n\
    auo Latn NG\n\
    aup Latn PG\n\
    auq Latn ID\n\
    aur Latn PG\n\
    aut Latn PF\n\
    auu Latn ID\n\
    auw Latn ID\n\
    auy Latn PG\n\
    auz Arab UZ\n\
    avb Latn PG\n\
    avd Arab IR\n\
    avi Latn CI\n\
    avk Latn 001\n\
    avl Arab EG\n\
    avm Latn AU\n\
    avn Latn GH\n\
    avo Latn BR\n\
    avs Latn PE\n\
    avt Latn PG\n\
    avu Latn SS\n\
    avv Latn BR\n\
    awb Latn PG\n\
    awc Latn NG\n\
    awe Latn BR\n\
    awg Latn AU\n\
    awh Latn ID\n\
    awi Latn PG\n\
    awk Latn AU\n\
    awm Latn PG\n\
    awn Ethi ET\n\
    awo Latn NG\n\
    awr Latn ID\n\
    aws Latn ID\n\
    awt Latn BR\n\
    awu Latn ID\n\
    awv Latn ID\n\
    aww Latn PG\n\
    awx Latn PG\n\
    awy Latn ID\n\
    axb Latn AR\n\
    axe Latn AU\n\
    axg Latn BR\n\
    axk Latn CF\n\
    axl Latn AU\n\
    axm Armn AM\n\
    axx Latn NC\n\
    aya Latn PG\n\
    ayb Latn BJ\n\
    ayc Latn PE\n\
    ayd Latn AU\n\
    aye Latn NG\n\
    ayg Latn TG\n\
    ayh Arab YE\n\
    ayi Latn NG\n\
    ayk Latn NG\n\
    ayl Arab LY\n\
    ayn Arab YE\n\
    ayo Latn PY\n\
    ayp Arab IQ\n\
    ayq Latn PG\n\
    ays Latn PH\n\
    ayt Latn PH\n\
    ayu Latn NG\n\
    ayz Latn ID\n\
    azb Arab IR\n\
    azd Latn MX\n\
    azg Latn MX\n\
    azm Latn MX\n\
    azn Latn MX\n\
    azo Latn CM\n\
    azt Latn PH\n\
    azz Latn MX\n\
    baa Latn SB\n\
    bab Latn GW\n\
    bac Latn ID\n\
    bae Latn VE\n\
    baf Latn CM\n\
    bag Latn CM\n\
    bah Latn BS\n\
    baj Latn ID\n\
    bao Latn CO\n\
    bau Latn NG\n\
    bav Latn CM\n\
    baw Latn CM\n\
    bay Latn ID\n\
    bba Latn BJ\n\
    bbb Latn PG\n\
    bbd Latn PG\n\
    bbe Latn CD\n\
    bbf Latn PG\n\
    bbg Latn GA\n\
    bbi Latn CM\n\
    bbk Latn CM\n\
    bbl Geor GE\n\
    bbm Latn CD\n\
    bbn Latn PG\n\
    bbo Latn BF\n\
    bbp Latn CF\n\
    bbq Latn CM\n\
    bbr Latn PG\n\
    bbs Latn NG\n\
    bbt Latn NG\n\
    bbu Latn NG\n\
    bbv Latn PG\n\
    bbw Latn CM\n\
    bbx Latn CM\n\
    bby Latn CM\n\
    bca Latn CN\n\
    bcb Latn SN\n\
    bcd Latn ID\n\
    bce Latn CM\n\
    bcf Latn PG\n\
    bcg Latn GN\n\
    bch Latn PG\n\
    bcj Latn AU\n\
    bck Latn AU\n\
    bcm Latn PG\n\
    bcn Latn NG\n\
    bco Latn PG\n\
    bcp Latn CD\n\
    bcq Ethi ET\n\
    bcr Latn CA\n\
    bcs Latn NG\n\
    bct Latn CD\n\
    bcu Latn PG\n\
    bcv Latn NG\n\
    bcw Latn CM\n\
    bcy Latn NG\n\
    bcz Latn SN\n\
    bda Latn SN\n\
    bdb Latn ID\n\
    bdc Latn CO\n\
    bdd Latn PG\n\
    bde Latn NG\n\
    bdf Latn PG\n\
    bdg Latn MY\n\
    bdh Latn SS\n\
    bdi Latn SD\n\
    bdj Latn SS\n\
    bdk Latn AZ\n\
    bdl Latn ID\n\
    bdm Latn TD\n\
    bdn Latn CM\n\
    bdo Latn TD\n\
    bdp Latn TZ\n\
    bdq Latn VN\n\
    bdr Latn MY\n\
    bds Latn TZ\n\
    bdt Latn CF\n\
    bdu Latn CM\n\
    bdv Orya IN\n\
    bdw Latn ID\n\
    bdx Latn ID\n\
    bdy Latn AU\n\
    bdz Arab PK\n\
    bea Latn CA\n\
    beb Latn CM\n\
    bec Latn CM\n\
    bed Latn ID\n\
    bee Deva IN\n\
    bef Latn PG\n\
    beh Latn BJ\n\
    bei Latn ID\n\
    bek Latn PG\n\
    beo Latn PG\n\
    bep Latn ID\n\
    beq Latn CG\n\
    bes Latn TD\n\
    bet Latn CI\n\
    beu Latn ID\n\
    bev Latn CI\n\
    bex Latn SS\n\
    bey Latn PG\n\
    bfa Latn SS\n\
    bfb Deva IN\n\
    bfc Latn CN\n\
    bfe Latn ID\n\
    bff Latn CF\n\
    bfg Latn ID\n\
    bfh Latn PG\n\
    bfj Latn CM\n\
    bfl Latn CF\n\
    bfm Latn CM\n\
    bfn Latn TL\n\
    bfo Latn BF\n\
    bfp Latn CM\n\
    bfs Latn CN\n\
    bfu Tibt IN\n\
    bfw Orya IN\n\
    bfx Latn PH\n\
    bfz Deva IN\n\
    bga Latn NG\n\
    bgb Latn ID\n\
    bgd Deva IN\n\
    bgf Latn CM\n\
    bgg Latn IN\n\
    bgi Latn PH\n\
    bgj Latn CM\n\
    bgo Latn GN\n\
    bgp Arab PK\n\
    bgq Deva IN\n\
    bgr Latn IN\n\
    bgs Latn PH\n\
    bgt Latn SB\n\
    bgu Latn NG\n\
    bgv Latn ID\n\
    bgw Deva IN\n\
    bgy Latn ID\n\
    bgz Latn ID\n\
    bha Deva IN\n\
    bhc Latn ID\n\
    bhd Deva IN\n\
    bhe Arab PK\n\
    bhf Latn PG\n\
    bhg Latn PG\n\
    bhh Cyrl IL\n\
    bhj Deva NP\n\
    bhl Latn PG\n\
    bhm Arab OM\n\
    bhn Syrc GE\n\
    bhp Latn ID\n\
    bhq Latn ID\n\
    bhr Latn MG\n\
    bhs Latn CM\n\
    bht Deva IN\n\
    bhu Deva IN\n\
    bhv Latn ID\n\
    bhw Latn ID\n\
    bhy Latn CD\n\
    bhz Latn ID\n\
    bia Latn AU\n\
    bib Latn BF\n\
    bid Latn TD\n\
    bie Latn PG\n\
    bif Latn GW\n\
    big Latn PG\n\
    bil Latn NG\n\
    bim Latn GH\n\
    bio Latn PG\n\
    bip Latn CD\n\
    biq Latn PG\n\
    bir Latn PG\n\
    bit Latn PG\n\
    biu Latn IN\n\
    biv Latn GH\n\
    biw Latn CM\n\
    biy Deva IN\n\
    biz Latn CD\n\
    bja Latn CD\n\
    bjb Latn AU\n\
    bjc Latn PG\n\
    bjf Syrc IL\n\
    bjg Latn GW\n\
    bjh Latn PG\n\
    bji Latn ET\n\
    bjk Latn PG\n\
    bjl Latn PG\n\
    bjm Arab IQ\n\
    bjo Latn CF\n\
    bjp Latn PG\n\
    bjr Latn PG\n\
    bjs Latn BB\n\
    bju Latn CM\n\
    bjv Latn TD\n\
    bjw Latn CI\n\
    bjx Latn PH\n\
    bjy Latn AU\n\
    bjz Latn PG\n\
    bka Latn NG\n\
    bkc Latn CM\n\
    bkd Latn PH\n\
    bkf Latn CD\n\
    bkg Latn CF\n\
    bkh Latn CM\n\
    bki Latn VU\n\
    bkj Latn CF\n\
    bkk Tibt IN\n\
    bkl Latn ID\n\
    bkn Latn ID\n\
    bko Latn CM\n\
    bkp Latn CD\n\
    bkq Latn BR\n\
    bkr Latn ID\n\
    bks Latn PH\n\
    bkt Latn CD\n\
    bkv Latn NG\n\
    bkw Latn CG\n\
    bkx Latn TL\n\
    bky Latn NG\n\
    bkz Latn ID\n\
    blb Latn SB\n\
    blc Latn CA\n\
    bld Latn ID\n\
    ble Latn GW\n\
    blf Latn ID\n\
    blh Latn LR\n\
    bli Latn CD\n\
    blj Latn ID\n\
    blk Mymr MM\n\
    blm Latn SS\n\
    bln Latn PH\n\
    blp Latn SB\n\
    blq Latn PG\n\
    blr Latn CN\n\
    bls Latn ID\n\
    blv Latn AO\n\
    blw Latn PH\n\
    blx Latn PH\n\
    bly Latn BJ\n\
    blz Latn ID\n\
    bma Latn NG\n\
    bmb Latn CD\n\
    bmc Latn PG\n\
    bmd Latn GN\n\
    bme Latn CF\n\
    bmf Latn SL\n\
    bmg Latn CD\n\
    bmh Latn PG\n\
    bmi Latn TD\n\
    bmj Deva NP\n\
    bmk Latn PG\n\
    bml Latn CD\n\
    bmm Latn MG\n\
    bmn Latn PG\n\
    bmo Latn CM\n\
    bmp Latn PG\n\
    bmr Latn CO\n\
    bms Latn NE\n\
    bmu Latn PG\n\
    bmv Latn CM\n\
    bmw Latn CG\n\
    bmx Latn PG\n\
    bmz Latn PG\n\
    bna Latn ID\n\
    bnb Latn MY\n\
    bnc Latn PH\n\
    bnd Latn ID\n\
    bne Latn ID\n\
    bnf Latn ID\n\
    bng Latn GQ\n\
    bni Latn CD\n\
    bnj Latn PH\n\
    bnk Latn VU\n\
    bnm Latn GQ\n\
    bnn Latn TW\n\
    bno Latn PH\n\
    bnp Latn PG\n\
    bnq Latn ID\n\
    bnr Latn VU\n\
    bns Deva IN\n\
    bnu Latn ID\n\
    bnv Latn ID\n\
    bnw Latn PG\n\
    bnx Latn CD\n\
    bny Latn MY\n\
    bnz Latn CM\n\
    boa Latn PE\n\
    bob Latn KE\n\
    boe Latn CM\n\
    bof Latn BF\n\
    boh Latn CD\n\
    boj Latn PG\n\
    bok Latn CG\n\
    bol Latn NG\n\
    bom Latn NG\n\
    bon Latn PG\n\
    boo Latn ML\n\
    bop Latn PG\n\
    boq Latn PG\n\
    bor Latn BR\n\
    bot Latn SS\n\
    bou Latn TZ\n\
    bov Latn GH\n\
    bow Latn PG\n\
    box Latn BF\n\
    boy Latn CF\n\
    boz Latn ML\n\
    bpa Latn VU\n\
    bpc Latn CM\n\
    bpd Latn CF\n\
    bpe Latn PG\n\
    bpg Latn ID\n\
    bph Cyrl RU\n\
    bpi Latn PG\n\
    bpj Latn CD\n\
    bpk Latn NC\n\
    bpl Latn AU\n\
    bpm Latn PG\n\
    bpo Latn ID\n\
    bpp Latn ID\n\
    bpq Latn ID\n\
    bpr Latn PH\n\
    bps Latn PH\n\
    bpt Latn AU\n\
    bpu Latn PG\n\
    bpv Latn ID\n\
    bpw Latn PG\n\
    bpx Deva IN\n\
    bpz Latn ID\n\
    bqa Latn BJ\n\
    bqb Latn ID\n\
    bqc Latn BJ\n\
    bqd Latn CM\n\
    bqf Latn GN\n\
    bqg Latn TG\n\
    bqj Latn SN\n\
    bqk Latn CF\n\
    bql Latn PG\n\
    bqm Latn CM\n\
    bqo Latn CM\n\
    bqp Latn NG\n\
    bqq Latn ID\n\
    bqr Latn ID\n\
    bqs Latn PG\n\
    bqt Latn CM\n\
    bqu Latn CD\n\
    bqw Latn NG\n\
    bqx Latn NG\n\
    bqz Latn CM\n\
    brb Khmr KH\n\
    brc Latn GY\n\
    brd Deva NP\n\
    brf Latn CD\n\
    brg Latn BO\n\
    bri Latn CM\n\
    brj Latn VU\n\
    brk Arab SD\n\
    brl Latn BW\n\
    brm Latn CD\n\
    brn Latn CR\n\
    bro Tibt BT\n\
    brp Latn ID\n\
    brq Latn PG\n\
    brr Latn SB\n\
    brs Latn ID\n\
    brt Latn NG\n\
    bru Latn VN\n\
    brv Laoo LA\n\
    brw Knda IN\n\
    bry Latn PG\n\
    brz Latn PG\n\
    bsa Latn ID\n\
    bsb Latn BN\n\
    bse Latn CM\n\
    bsf Latn NG\n\
    bsh Arab AF\n\
    bsi Latn CM\n\
    bsj Latn NG\n\
    bsk Arab PK\n\
    bsl Latn NG\n\
    bsm Latn ID\n\
    bsn Latn CO\n\
    bso Latn TD\n\
    bsp Latn GN\n\
    bsr Latn NG\n\
    bst Ethi ET\n\
    bsu Latn ID\n\
    bsv Latn GN\n\
    bsw Latn ET\n\
    bsx Latn NG\n\
    bsy Latn MY\n\
    bta Latn NG\n\
    btc Latn CM\n\
    btd Batk ID\n\
    bte Latn NG\n\
    btf Latn TD\n\
    btg Latn CI\n\
    bth Latn MY\n\
    bti Latn ID\n\
    btj Latn ID\n\
    btm Batk ID\n\
    btn Latn PH\n\
    btp Latn PG\n\
    btq Latn MY\n\
    btr Latn VU\n\
    bts Latn ID\n\
    btt Latn NG\n\
    btu Latn NG\n\
    btw Latn PH\n\
    btx Latn ID\n\
    bty Latn ID\n\
    btz Latn ID\n\
    bub Latn TD\n\
    bud Latn TG\n\
    bue Latn CA\n\
    buf Latn CD\n\
    buh Latn CN\n\
    bui Latn CG\n\
    buj Latn NG\n\
    buk Latn PG\n\
    bun Latn SL\n\
    buo Latn PG\n\
    bup Latn ID\n\
    buq Latn PG\n\
    bus Latn NG\n\
    but Latn PG\n\
    buu Latn CD\n\
    buv Latn PG\n\
    buw Latn GA\n\
    bux Latn NG\n\
    buy Latn SL\n\
    buz Latn NG\n\
    bva Latn TD\n\
    bvc Latn SB\n\
    bvd Latn SB\n\
    bve Latn ID\n\
    bvf Latn TD\n\
    bvg Latn CM\n\
    bvh Latn NG\n\
    bvi Latn SS\n\
    bvj Latn NG\n\
    bvk Latn ID\n\
    bvm Latn CM\n\
    bvn Latn PG\n\
    bvo Latn TD\n\
    bvq Latn CF\n\
    bvr Latn AU\n\
    bvt Latn ID\n\
    bvu Latn ID\n\
    bvv Latn VE\n\
    bvw Latn NG\n\
    bvx Latn CG\n\
    bvy Latn PH\n\
    bvz Latn ID\n\
    bwa Latn NC\n\
    bwb Latn FJ\n\
    bwc Latn ZM\n\
    bwd Latn PG\n\
    bwe Mymr MM\n\
    bwf Latn PG\n\
    bwg Latn MZ\n\
    bwh Latn CM\n\
    bwi Latn VE\n\
    bwj Latn BF\n\
    bwk Latn PG\n\
    bwl Latn CD\n\
    bwm Latn PG\n\
    bwo Latn ET\n\
    bwp Latn ID\n\
    bwq Latn BF\n\
    bwr Latn NG\n\
    bws Latn CD\n\
    bwt Latn CM\n\
    bwu Latn GH\n\
    bww Latn CD\n\
    bwx Latn CN\n\
    bwy Latn BF\n\
    bwz Latn CG\n\
    bxa Latn SB\n\
    bxb Latn SS\n\
    bxc Latn GQ\n\
    bxf Latn PG\n\
    bxg Latn CD\n\
    bxh Latn PG\n\
    bxi Latn AU\n\
    bxj Latn AU\n\
    bxl Latn BF\n\
    bxm Cyrl MN\n\
    bxn Latn AU\n\
    bxo Latn NG\n\
    bxp Latn CM\n\
    bxq Latn NG\n\
    bxs Latn CM\n\
    bxu Mong CN\n\
    bxv Latn TD\n\
    bxw Latn ML\n\
    bxz Latn PG\n\
    bya Latn PH\n\
    byb Latn CM\n\
    byc Latn NG\n\
    byd Latn ID\n\
    bye Latn PG\n\
    byf Latn NG\n\
    byh Deva NP\n\
    byi Latn CD\n\
    byj Latn NG\n\
    byk Latn CN\n\
    byl Latn ID\n\
    bym Latn AU\n\
    byp Latn NG\n\
    byr Latn PG\n\
    bys Latn NG\n\
    byw Deva NP\n\
    byx Latn PG\n\
    byz Latn PG\n\
    bza Latn LR\n\
    bzb Latn ID\n\
    bzc Latn MG\n\
    bzd Latn CR\n\
    bzf Latn PG\n\
    bzh Latn PG\n\
    bzi Thai TH\n\
    bzj Latn BZ\n\
    bzk Latn NI\n\
    bzl Latn ID\n\
    bzm Latn CD\n\
    bzn Latn ID\n\
    bzo Latn CD\n\
    bzp Latn ID\n\
    bzq Latn ID\n\
    bzr Latn AU\n\
    bzt Latn 001\n\
    bzu Latn ID\n\
    bzv Latn CM\n\
    bzw Latn NG\n\
    bzx Latn ML\n\
    bzy Latn NG\n\
    bzz Latn NG\n\
    caa Latn GT\n\
    cab Latn HN\n\
    cac Latn GT\n\
    cae Latn SN\n\
    caf Latn CA\n\
    cag Latn PY\n\
    cah Latn PE\n\
    caj Latn BO\n\
    cak Latn GT\n\
    cal Latn MP\n\
    cam Latn NC\n\
    can Latn PG\n\
    cao Latn BO\n\
    cap Latn BO\n\
    caq Latn IN\n\
    car Latn VE\n\
    cas Latn BO\n\
    cav Latn BO\n\
    caw Latn BO\n\
    cax Latn BO\n\
    cay Latn CA\n\
    caz Latn BO\n\
    cbb Latn CO\n\
    cbc Latn CO\n\
    cbd Latn CO\n\
    cbg Latn CO\n\
    cbi Latn EC\n\
    cbj Latn BJ\n\
    cbk Latn PH\n\
    cbl Latn MM\n\
    cbn Thai TH\n\
    cbo Latn NG\n\
    cbq Latn NG\n\
    cbr Latn PE\n\
    cbs Latn PE\n\
    cbt Latn PE\n\
    cbu Latn PE\n\
    cbv Latn CO\n\
    cbw Latn PH\n\
    cby Latn CO\n\
    ccc Latn PE\n\
    ccd Latn BR\n\
    cce Latn MZ\n\
    ccg Latn NG\n\
    ccj Latn GW\n\
    ccl Latn TZ\n\
    ccm Latn MY\n\
    cco Latn MX\n\
    cde Telu IN\n\
    cdf Latn IN\n\
    cdh Deva IN\n\
    cdi Gujr IN\n\
    cdj Deva IN\n\
    cdm Deva NP\n\
    cdo Hans CN\n\
    cdr Latn NG\n\
    cdz Beng IN\n\
    cea Latn US\n\
    ceg Latn PY\n\
    cek Latn MM\n\
    cen Latn NG\n\
    cet Latn NG\n\
    cey Latn MM\n\
    cfa Latn NG\n\
    cfd Latn NG\n\
    cfg Latn NG\n\
    cfm Latn MM\n\
    cga Latn PG\n\
    cgc Latn PH\n\
    cgk Tibt BT\n\
    chb Latn CO\n\
    chd Latn MX\n\
    chf Latn MX\n\
    chg Arab TM\n\
    chh Latn US\n\
    chj Latn MX\n\
    chl Latn US\n\
    chn Latn US\n\
    chq Latn MX\n\
    cht Latn PE\n\
    chw Latn MZ\n\
    chx Deva NP\n\
    chy Latn US\n\
    chz Latn MX\n\
    cia Latn ID\n\
    cib Latn BJ\n\
    cie Latn NG\n\
    cih Deva IN\n\
    cim Latn IT\n\
    cin Latn BR\n\
    cip Latn MX\n\
    cir Latn NC\n\
    ciw Latn US\n\
    ciy Latn VE\n\
    cje Latn VN\n\
    cjh Latn US\n\
    cji Cyrl RU\n\
    cjk Latn AO\n\
    cjn Latn PG\n\
    cjo Latn PE\n\
    cjp Latn CR\n\
    cjs Latn RU\n\
    cjv Latn PG\n\
    cjy Hans CN\n\
    ckl Latn NG\n\
    ckm Latn HR\n\
    ckn Latn MM\n\
    cko Latn GH\n\
    ckq Latn TD\n\
    ckr Latn PG\n\
    cks Latn NC\n\
    ckt Cyrl RU\n\
    cku Latn US\n\
    ckv Latn TW\n\
    ckx Latn CM\n\
    cky Latn NG\n\
    ckz Latn GT\n\
    cla Latn NG\n\
    cle Latn MX\n\
    clh Arab PK\n\
    cli Latn GH\n\
    clj Latn MM\n\
    clk Latn IN\n\
    cll Latn GH\n\
    clm Latn US\n\
    clo Latn MX\n\
    clt Latn MM\n\
    clu Latn PH\n\
    clw Cyrl RU\n\
    cly Latn MX\n\
    cma Latn VN\n\
    cme Latn BF\n\
    cmi Latn CO\n\
    cml Latn ID\n\
    cmo Latn VN\n\
    cmr Latn MM\n\
    cms Latn IT\n\
    cmt Latn ZA\n\
    cna Tibt IN\n\
    cnb Latn MM\n\
    cnc Latn VN\n\
    cng Latn CN\n\
    cnh Latn MM\n\
    cni Latn PE\n\
    cnk Latn MM\n\
    cnl Latn MX\n\
    cnp Hans CN\n\
    cnq Latn CM\n\
    cns Latn ID\n\
    cnt Latn MX\n\
    cnw Latn MM\n\
    cnx Latn GB\n\
    coa Latn AU\n\
    cob Latn MX\n\
    coc Latn MX\n\
    cod Latn PE\n\
    coe Latn CO\n\
    cof Latn EC\n\
    cog Thai TH\n\
    coh Latn KE\n\
    coj Latn MX\n\
    cok Latn MX\n\
    col Latn US\n\
    com Latn US\n\
    coo Latn CA\n\
    coq Latn US\n\
    cot Latn PE\n\
    cou Latn SN\n\
    cox Latn PE\n\
    coz Latn MX\n\
    cpa Latn MX\n\
    cpb Latn PE\n\
    cpc Latn PE\n\
    cpg Grek GR\n\
    cpi Latn NR\n\
    cpn Latn GH\n\
    cpo Latn BF\n\
    cpu Latn PE\n\
    cpx Latn CN\n\
    cpy Latn PE\n\
    cqd Latn CN\n\
    cra Latn ET\n\
    crb Latn VC\n\
    crc Latn VU\n\
    crd Latn US\n\
    crf Latn CO\n\
    cri Latn ST\n\
    crj Cans CA\n\
    crm Cans CA\n\
    crn Latn MX\n\
    cro Latn US\n\
    crq Latn AR\n\
    crt Latn AR\n\
    crv Latn IN\n\
    crw Latn VN\n\
    crx Latn CA\n\
    cry Latn NG\n\
    crz Latn US\n\
    csa Latn MX\n\
    csh Mymr MM\n\
    csj Latn MM\n\
    csk Latn SN\n\
    csm Latn US\n\
    cso Latn MX\n\
    csp Hans CN\n\
    css Latn US\n\
    cst Latn US\n\
    csv Latn MM\n\
    csy Latn MM\n\
    csz Latn US\n\
    cta Latn MX\n\
    ctc Latn US\n\
    cte Latn MX\n\
    ctg Beng BD\n\
    cth Latn MM\n\
    ctl Latn MX\n\
    ctm Latn US\n\
    ctn Deva NP\n\
    cto Latn CO\n\
    ctp Latn MX\n\
    cts Latn PH\n\
    ctt Taml IN\n\
    ctu Latn MX\n\
    cty Taml IN\n\
    ctz Latn MX\n\
    cua Latn VN\n\
    cub Latn CO\n\
    cuc Latn MX\n\
    cuh Latn KE\n\
    cui Latn CO\n\
    cuj Latn PE\n\
    cuk Latn PA\n\
    cul Latn BR\n\
    cuo Latn VE\n\
    cup Latn US\n\
    cut Latn MX\n\
    cuu Lana CN\n\
    cuv Latn CM\n\
    cux Latn MX\n\
    cuy Latn MX\n\
    cvg Latn IN\n\
    cvn Latn MX\n\
    cwa Latn TZ\n\
    cwb Latn MZ\n\
    cwe Latn TZ\n\
    cwg Latn MY\n\
    cwt Latn SN\n\
    cxh Latn NG\n\
    cya Latn MX\n\
    cyb Latn BO\n\
    cyo Latn PH\n\
    czh Hans CN\n\
    czk Hebr CZ\n\
    czn Latn MX\n\
    czt Latn MM\n\
    daa Latn TD\n\
    dac Latn PG\n\
    dad Latn PG\n\
    dae Latn CM\n\
    dag Latn GH\n\
    dah Latn PG\n\
    dai Latn TD\n\
    daj Latn SD\n\
    dal Latn KE\n\
    dam Latn NG\n\
    dao Latn MM\n\
    daq Deva IN\n\
    das Latn CI\n\
    dau Latn TD\n\
    daw Latn PH\n\
    dax Latn AU\n\
    daz Latn ID\n\
    dba Latn ML\n\
    dbb Latn NG\n\
    dbd Latn NG\n\
    dbe Latn ID\n\
    dbf Latn ID\n\
    dbg Latn ML\n\
    dbi Latn NG\n\
    dbj Latn MY\n\
    dbl Latn AU\n\
    dbm Latn NG\n\
    dbn Latn ID\n\
    dbo Latn NG\n\
    dbp Latn NG\n\
    dbq Latn CM\n\
    dbt Latn ML\n\
    dbu Latn ML\n\
    dbv Latn NG\n\
    dbw Latn ML\n\
    dby Latn PG\n\
    dcr Latn VI\n\
    dda Latn AU\n\
    ddd Latn SS\n\
    dde Latn CG\n\
    ddg Latn TL\n\
    ddi Latn PG\n\
    ddj Latn AU\n\
    ddn Latn BJ\n\
    ddo Cyrl RU\n\
    ddr Latn AU\n\
    dds Latn ML\n\
    ddw Latn ID\n\
    dec Latn SD\n\
    ded Latn PG\n\
    dee Latn LR\n\
    def Arab IR\n\
    deg Latn NG\n\
    deh Arab PK\n\
    dei Latn ID\n\
    dek Latn CM\n\
    del Latn US\n\
    dem Latn ID\n\
    deq Latn CF\n\
    der Beng IN\n\
    des Latn BR\n\
    dev Latn PG\n\
    dez Latn CD\n\
    dga Latn GH\n\
    dgb Latn ML\n\
    dgc Latn PH\n\
    dgd Latn BF\n\
    dge Latn PG\n\
    dgg Latn PG\n\
    dgh Latn NG\n\
    dgi Latn BF\n\
    dgk Latn CF\n\
    dgl Arab SD\n\
    dgn Latn AU\n\
    dgs Latn BF\n\
    dgt Latn AU\n\
    dgw Latn AU\n\
    dgx Latn PG\n\
    dgz Latn PG\n\
    dhg Latn AU\n\
    dhi Deva NP\n\
    dhl Latn AU\n\
    dhm Latn AO\n\
    dhn Gujr IN\n\
    dho Deva IN\n\
    dhr Latn AU\n\
    dhs Latn TZ\n\
    dhu Latn AU\n\
    dhv Latn NC\n\
    dhw Deva NP\n\
    dhx Latn AU\n\
    dia Latn PG\n\
    dib Latn SS\n\
    dic Latn CI\n\
    did Latn SS\n\
    dif Latn AU\n\
    dig Latn KE\n\
    dih Latn MX\n\
    dii Latn CM\n\
    dij Latn ID\n\
    dil Latn SD\n\
    din Latn SS\n\
    dio Latn NG\n\
    dip Latn SS\n\
    dir Latn NG\n\
    dis Latn IN\n\
    diu Latn NA\n\
    diw Latn SS\n\
    dix Latn VU\n\
    diy Latn ID\n\
    diz Latn CD\n\
    dja Latn AU\n\
    djb Latn AU\n\
    djc Latn TD\n\
    djd Latn AU\n\
    djf Latn AU\n\
    dji Latn AU\n\
    djj Latn AU\n\
    djk Latn SR\n\
    djm Latn ML\n\
    djn Latn AU\n\
    djo Latn ID\n\
    djr Latn AU\n\
    dju Latn PG\n\
    djw Latn AU\n\
    dka Tibt BT\n\
    dkg Latn NG\n\
    dkk Latn ID\n\
    dkr Latn MY\n\
    dks Latn SS\n\
    dkx Latn CM\n\
    dlg Cyrl RU\n\
    dlm Latn HR\n\
    dln Latn IN\n\
    dma Latn GA\n\
    dmb Latn ML\n\
    dmc Latn PG\n\
    dmd Latn AU\n\
    dme Latn CM\n\
    dmg Latn MY\n\
    dmk Arab PK\n\
    dml Arab PK\n\
    dmm Latn CM\n\
    dmo Latn CM\n\
    dmr Latn ID\n\
    dms Latn ID\n\
    dmu Latn ID\n\
    dmv Latn MY\n\
    dmw Latn AU\n\
    dmx Latn MZ\n\
    dmy Latn ID\n\
    dna Latn ID\n\
    dnd Latn PG\n\
    dne Latn TZ\n\
    dng Cyrl KG\n\
    dni Latn ID\n\
    dnk Latn ID\n\
    dnn Latn BF\n\
    dno Latn CD\n\
    dnr Latn PG\n\
    dnt Latn ID\n\
    dnu Mymr MM\n\
    dnv Mymr MM\n\
    dnw Latn ID\n\
    dny Latn BR\n\
    doa Latn PG\n\
    dob Latn PG\n\
    doc Latn CN\n\
    doe Latn TZ\n\
    dof Latn PG\n\
    doh Latn NG\n\
    dok Latn ID\n\
    dol Latn PG\n\
    don Latn PG\n\
    doo Latn CD\n\
    dop Latn BJ\n\
    dor Latn SB\n\
    dos Latn BF\n\
    dot Latn NG\n\
    dov Latn ZW\n\
    dow Latn CM\n\
    dox Ethi ET\n\
    doy Latn GH\n\
    dpp Latn MY\n\
    drc Latn PT\n\
    dre Tibt NP\n\
    drg Latn MY\n\
    dri Latn NG\n\
    drl Latn AU\n\
    drn Latn ID\n\
    dro Latn MY\n\
    drq Deva NP\n\
    drs Ethi ET\n\
    drt Latn NL\n\
    dru Latn TW\n\
    dry Deva NP\n\
    dsh Latn KE\n\
    dsi Latn TD\n\
    dsk Latn NG\n\
    dsn Latn ID\n\
    dso Orya IN\n\
    dsq Latn ML\n\
    dta Latn CN\n\
    dtb Latn MY\n\
    dtd Latn CA\n\
    dth Latn AU\n\
    dti Latn ML\n\
    dtk Latn ML\n\
    dto Latn ML\n\
    dtr Latn MY\n\
    dts Latn ML\n\
    dtt Latn ML\n\
    dtu Latn ML\n\
    dub Gujr IN\n\
    duc Latn PG\n\
    due Latn PH\n\
    duf Latn NC\n\
    dug Latn KE\n\
    duh Deva IN\n\
    dui Latn PG\n\
    duk Latn PG\n\
    dul Latn PH\n\
    dum Latn NL\n\
    dun Latn ID\n\
    duo Latn PH\n\
    dup Latn ID\n\
    duq Latn ID\n\
    dur Latn CM\n\
    dus Deva NP\n\
    duu Latn CN\n\
    duv Latn ID\n\
    duw Latn ID\n\
    dux Latn ML\n\
    duy Latn PH\n\
    duz Latn CM\n\
    dva Latn PG\n\
    dwa Latn NG\n\
    dwk Orya IN\n\
    dwr Latn ET\n\
    dws Latn 001\n\
    dwu Latn AU\n\
    dww Latn PG\n\
    dwy Latn AU\n\
    dwz Deva NP\n\
    dya Latn BF\n\
    dyb Latn AU\n\
    dyd Latn AU\n\
    dyg Latn PH\n\
    dyi Latn CI\n\
    dym Latn ML\n\
    dyn Latn AU\n\
    dyr Latn NG\n\
    dyy Latn AU\n\
    dza Latn NG\n\
    dzd Latn NG\n\
    dze Latn AU\n\
    dzg Latn TD\n\
    dzl Tibt BT\n\
    dzn Latn CD\n\
    eaa Latn AU\n\
    ebc Latn ID\n\
    ebg Latn NG\n\
    ebk Latn PH\n\
    ebo Latn CG\n\
    ebr Latn CI\n\
    ecr Grek GR\n\
    efa Latn NG\n\
    efe Latn CD\n\
    ega Latn CI\n\
    egm Latn TZ\n\
    ego Latn NG\n\
    ehu Latn NG\n\
    eip Latn ID\n\
    eit Latn PG\n\
    eiv Latn PG\n\
    eja Latn GW\n\
    eka Latn NG\n\
    eke Latn NG\n\
    ekg Latn ID\n\
    eki Latn NG\n\
    ekl Latn BD\n\
    ekm Latn CM\n\
    eko Latn MZ\n\
    ekp Latn NG\n\
    ekr Latn NG\n\
    ele Latn PG\n\
    elk Latn PG\n\
    elm Latn NG\n\
    elo Latn KE\n\
    elu Latn PG\n\
    ema Latn NG\n\
    emb Latn ID\n\
    eme Latn GF\n\
    emg Deva NP\n\
    emi Latn PG\n\
    emm Latn MX\n\
    emn Latn CM\n\
    emp Latn PA\n\
    ems Latn US\n\
    emu Deva IN\n\
    emw Latn ID\n\
    emx Latn FR\n\
    emz Latn CM\n\
    ena Latn PG\n\
    enb Latn KE\n\
    enc Latn VN\n\
    end Latn ID\n\
    enf Cyrl RU\n\
    enh Cyrl RU\n\
    enl Latn PY\n\
    enm Latn GB\n\
    enn Latn NG\n\
    eno Latn ID\n\
    enq Latn PG\n\
    enr Latn ID\n\
    env Latn NG\n\
    enw Latn NG\n\
    enx Latn PY\n\
    eot Latn CI\n\
    epi Latn NG\n\
    era Taml IN\n\
    erg Latn VU\n\
    erh Latn NG\n\
    eri Latn PG\n\
    erk Latn VU\n\
    err Latn AU\n\
    ers Latn CN\n\
    ert Latn ID\n\
    erw Latn ID\n\
    ese Latn BO\n\
    esh Arab IR\n\
    esi Latn US\n\
    esm Latn CI\n\
    ess Latn US\n\
    esy Latn PH\n\
    etb Latn NG\n\
    etn Latn VU\n\
    eto Latn CM\n\
    etr Latn PG\n\
    ets Latn NG\n\
    etu Latn NG\n\
    etx Latn NG\n\
    etz Latn ID\n\
    eud Latn MX\n\
    eve Cyrl RU\n\
    evh Latn NG\n\
    evn Cyrl RU\n\
    eya Latn US\n\
    eyo Latn KE\n\
    eza Latn NG\n\
    eze Latn NG\n\
    faa Latn PG\n\
    fab Latn GQ\n\
    fad Latn PG\n\
    faf Latn SB\n\
    fag Latn PG\n\
    fah Latn NG\n\
    fai Latn PG\n\
    faj Latn PG\n\
    fak Latn CM\n\
    fal Latn CM\n\
    fam Latn NG\n\
    fap Latn SN\n\
    far Latn SB\n\
    fau Latn ID\n\
    fax Latn ES\n\
    fay Arab IR\n\
    faz Arab IR\n\
    fer Latn SS\n\
    ffi Latn PG\n\
    fgr Latn TD\n\
    fie Latn NG\n\
    fif Latn SA\n\
    fip Latn TZ\n\
    fir Latn NG\n\
    fiw Latn PG\n\
    fkk Latn NG\n\
    fkv Latn NO\n\
    fla Latn US\n\
    flh Latn ID\n\
    fli Latn NG\n\
    fll Latn CM\n\
    fln Latn AU\n\
    flr Latn CD\n\
    fly Latn ZA\n\
    fmp Latn CM\n\
    fmu Deva IN\n\
    fnb Latn VU\n\
    fng Latn ZA\n\
    fni Latn TD\n\
    fod Latn BJ\n\
    foi Latn PG\n\
    fom Latn CD\n\
    for Latn PG\n\
    fos Latn TW\n\
    fpe Latn GQ\n\
    fqs Latn PG\n\
    frd Latn ID\n\
    frk Latn DE\n\
    frm Latn FR\n\
    fro Latn FR\n\
    frq Latn PG\n\
    frt Latn VU\n\
    fue Latn BJ\n\
    fuh Latn NE\n\
    fui Latn TD\n\
    fum Latn NG\n\
    fun Latn BR\n\
    fut Latn VU\n\
    fuu Latn CD\n\
    fuy Latn PG\n\
    fwa Latn NC\n\
    fwe Latn NA\n\
    gab Latn TD\n\
    gac Latn IN\n\
    gad Latn PH\n\
    gae Latn VE\n\
    gaf Latn PG\n\
    gah Latn PG\n\
    gai Latn PG\n\
    gaj Latn PG\n\
    gak Latn ID\n\
    gal Latn TL\n\
    gam Latn PG\n\
    gao Latn PG\n\
    gap Latn PG\n\
    gaq Orya IN\n\
    gar Latn PG\n\
    gas Gujr IN\n\
    gat Latn PG\n\
    gau Telu IN\n\
    gaw Latn PG\n\
    gax Latn ET\n\
    gba Latn CF\n\
    gbb Latn AU\n\
    gbd Latn AU\n\
    gbe Latn PG\n\
    gbf Latn PG\n\
    gbg Latn CF\n\
    gbh Latn BJ\n\
    gbi Latn ID\n\
    gbj Orya IN\n\
    gbk Deva IN\n\
    gbl Gujr IN\n\
    gbn Latn SS\n\
    gbp Latn CF\n\
    gbq Latn CF\n\
    gbr Latn NG\n\
    gbs Latn BJ\n\
    gbu Latn AU\n\
    gbv Latn CF\n\
    gbw Latn AU\n\
    gbx Latn BJ\n\
    gby Latn NG\n\
    gcc Latn PG\n\
    gcd Latn AU\n\
    gcf Latn GP\n\
    gcl Latn GD\n\
    gcn Latn PG\n\
    gct Latn VE\n\
    gdb Orya IN\n\
    gdc Latn AU\n\
    gdd Latn PG\n\
    gde Latn NG\n\
    gdf Latn NG\n\
    gdg Latn PH\n\
    gdh Latn AU\n\
    gdi Latn CF\n\
    gdj Latn AU\n\
    gdk Latn TD\n\
    gdl Latn ET\n\
    gdm Latn TD\n\
    gdn Latn PG\n\
    gdo Cyrl RU\n\
    gdq Latn YE\n\
    gdr Latn PG\n\
    gdt Latn AU\n\
    gdu Latn NG\n\
    gdx Deva IN\n\
    gea Latn NG\n\
    geb Latn PG\n\
    gec Latn LR\n\
    ged Latn NG\n\
    gef Latn ID\n\
    geg Latn NG\n\
    geh Latn CA\n\
    gei Latn ID\n\
    gej Latn TG\n\
    gek Latn NG\n\
    gel Latn NG\n\
    geq Latn CF\n\
    ges Latn ID\n\
    gev Latn GA\n\
    gew Latn NG\n\
    gex Latn SO\n\
    gey Latn CD\n\
    gfk Latn PG\n\
    gga Latn SB\n\
    ggb Latn LR\n\
    ggd Latn AU\n\
    gge Latn AU\n\
    ggg Arab PK\n\
    ggk Latn AU\n\
    ggl Latn PG\n\
    ggt Latn PG\n\
    ggu Latn CI\n\
    ggw Latn PG\n\
    gha Arab LY\n\
    ghc Latn GB\n\
    ghe Deva NP\n\
    ghk Latn MM\n\
    ghn Latn SB\n\
    gho Tfng MA\n\
    ghr Arab PK\n\
    ghs Latn PG\n\
    ght Tibt NP\n\
    gia Latn AU\n\
    gib Latn NG\n\
    gic Latn ZA\n\
    gid Latn CM\n\
    gie Latn CI\n\
    gig Arab PK\n\
    gih Latn AU\n\
    gim Latn PG\n\
    gin Cyrl RU\n\
    gip Latn PG\n\
    giq Latn VN\n\
    gir Latn VN\n\
    gis Latn CM\n\
    git Latn CA\n\
    gix Latn CD\n\
    giy Latn AU\n\
    giz Latn CM\n\
    gjm Latn AU\n\
    gjn Latn GH\n\
    gjr Latn AU\n\
    gka Latn PG\n\
    gkd Latn PG\n\
    gke Latn CM\n\
    gkn Latn NG\n\
    gko Latn AU\n\
    gkp Latn GN\n\
    gku Latn ZA\n\
    glb Latn NG\n\
    glc Latn TD\n\
    gld Cyrl RU\n\
    glh Arab AF\n\
    glj Latn TD\n\
    gll Latn AU\n\
    glo Latn NG\n\
    glr Latn LR\n\
    glu Latn TD\n\
    glw Latn NG\n\
    gma Latn AU\n\
    gmb Latn SB\n\
    gmd Latn NG\n\
    gmg Latn PG\n\
    gmh Latn DE\n\
    gml Latf DE\n\
    gmm Latn CM\n\
    gmn Latn CM\n\
    gmr Latn AU\n\
    gmu Latn PG\n\
    gmv Ethi ET\n\
    gmx Latn TZ\n\
    gmz Latn NG\n\
    gna Latn BF\n\
    gnb Latn IN\n\
    gnc Latn ES\n\
    gnd Latn CM\n\
    gne Latn NG\n\
    gng Latn TG\n\
    gnh Latn NG\n\
    gni Latn AU\n\
    gnj Latn CI\n\
    gnk Latn BW\n\
    gnl Latn AU\n\
    gnm Latn PG\n\
    gnn Latn AU\n\
    gnq Latn MY\n\
    gnr Latn AU\n\
    gnt Latn PG\n\
    gnu Latn PG\n\
    gnw Latn BO\n\
    gnz Latn CF\n\
    goa Latn CI\n\
    gob Latn CO\n\
    goc Latn PG\n\
    god Latn CI\n\
    goe Tibt BT\n\
    gof Ethi ET\n\
    gog Latn TZ\n\
    goh Latn DE\n\
    goi Latn PG\n\
    goj Deva IN\n\
    gok Deva IN\n\
    gol Latn LR\n\
    goo Latn FJ\n\
    gop Latn ID\n\
    goq Latn ID\n\
    gou Latn CM\n\
    gov Latn CI\n\
    gow Latn TZ\n\
    gox Latn CD\n\
    goy Latn TD\n\
    gpa Latn NG\n\
    gpe Latn GH\n\
    gpn Latn PG\n\
    gqa Latn NG\n\
    gqn Latn BR\n\
    gqr Latn TD\n\
    gra Deva IN\n\
    grb Latn LR\n\
    grd Latn NG\n\
    grg Latn PG\n\
    grh Latn NG\n\
    gri Latn SB\n\
    grj Latn LR\n\
    grm Latn MY\n\
    grq Latn PG\n\
    grs Latn ID\n\
    gru Ethi ET\n\
    grv Latn LR\n\
    grw Latn PG\n\
    grx Latn PG\n\
    gry Latn LR\n\
    grz Latn PG\n\
    gsl Latn SN\n\
    gsn Latn PG\n\
    gso Latn CF\n\
    gsp Latn PG\n\
    gta Latn BR\n\
    gtu Latn AU\n\
    gua Latn NG\n\
    gud Latn CI\n\
    gue Latn AU\n\
    guf Latn AU\n\
    guh Latn CO\n\
    gui Latn BO\n\
    guk Latn ET\n\
    gul Latn US\n\
    gum Latn CO\n\
    gun Latn BR\n\
    guo Latn CO\n\
    gup Latn AU\n\
    guq Latn PY\n\
    gut Latn CR\n\
    guu Latn VE\n\
    guw Latn BJ\n\
    gux Latn BF\n\
    gva Latn PY\n\
    gvc Latn BR\n\
    gve Latn PG\n\
    gvf Latn PG\n\
    gvj Latn BR\n\
    gvl Latn TD\n\
    gvm Latn NG\n\
    gvn Latn AU\n\
    gvo Latn BR\n\
    gvp Latn BR\n\
    gvs Latn PG\n\
    gvy Latn AU\n\
    gwa Latn CI\n\
    gwb Latn NG\n\
    gwc Arab PK\n\
    gwd Latn ET\n\
    gwe Latn TZ\n\
    gwf Arab PK\n\
    gwg Latn NG\n\
    gwj Latn BW\n\
    gwm Latn AU\n\
    gwn Latn NG\n\
    gwr Latn UG\n\
    gwt Arab AF\n\
    gwu Latn AU\n\
    gww Latn AU\n\
    gwx Latn GH\n\
    gxx Latn CI\n\
    gyb Latn PG\n\
    gyd Latn AU\n\
    gye Latn NG\n\
    gyf Latn AU\n\
    gyg Latn CF\n\
    gyi Latn CM\n\
    gyl Latn ET\n\
    gym Latn PA\n\
    gyn Latn GY\n\
    gyo Deva NP\n\
    gyr Latn BO\n\
    gyy Latn AU\n\
    gyz Latn NG\n\
    gza Latn SD\n\
    gzi Arab IR\n\
    gzn Latn ID\n\
    haa Latn US\n\
    hac Arab IR\n\
    had Latn ID\n\
    hae Latn ET\n\
    hag Latn GH\n\
    hah Latn PG\n\
    hai Latn CA\n\
    haj Latn IN\n\
    hal Latn VN\n\
    ham Latn PG\n\
    han Latn TZ\n\
    hao Latn PG\n\
    hap Latn ID\n\
    haq Latn TZ\n\
    har Ethi ET\n\
    has Latn CA\n\
    hav Latn CD\n\
    hax Latn CA\n\
    hay Latn TZ\n\
    hba Latn CD\n\
    hbb Latn NG\n\
    hbn Latn SD\n\
    hbo Hebr IL\n\
    hbu Latn TL\n\
    hch Latn MX\n\
    hdy Ethi ET\n\
    hed Latn TD\n\
    heg Latn ID\n\
    heh Latn TZ\n\
    hei Latn CA\n\
    hem Latn CD\n\
    hgm Latn NA\n\
    hgw Latn PG\n\
    hhi Latn PG\n\
    hhr Latn SN\n\
    hhy Latn PG\n\
    hia Latn NG\n\
    hib Latn PE\n\
    hid Latn US\n\
    hig Latn NG\n\
    hih Latn PG\n\
    hii Takr IN\n\
    hij Latn CM\n\
    hik Latn ID\n\
    hio Latn BW\n\
    hir Latn BR\n\
    hit Xsux TR\n\
    hiw Latn VU\n\
    hix Latn BR\n\
    hji Latn ID\n\
    hka Latn TZ\n\
    hke Latn CD\n\
    hkh Arab IN\n\
    hkk Latn PG\n\
    hla Latn PG\n\
    hlb Deva IN\n\
    hld Latn VN\n\
    hlt Latn MM\n\
    hma Latn CN\n\
    hmb Latn ML\n\
    hmf Latn VN\n\
    hmj Bopo CN\n\
    hmm Latn CN\n\
    hmn Latn CN\n\
    hmp Latn CN\n\
    hmq Bopo CN\n\
    hmr Latn IN\n\
    hms Latn CN\n\
    hmt Latn PG\n\
    hmu Latn ID\n\
    hmv Latn VN\n\
    hmw Latn CN\n\
    hmy Latn CN\n\
    hmz Latn CN\n\
    hna Latn CM\n\
    hng Latn AO\n\
    hnh Latn BW\n\
    hni Latn CN\n\
    hns Latn SR\n\
    hoa Latn SB\n\
    hob Latn PG\n\
    hod Latn NG\n\
    hoe Latn NG\n\
    hoh Arab OM\n\
    hoi Latn US\n\
    hol Latn AO\n\
    hom Latn SS\n\
    hoo Latn CD\n\
    hop Latn US\n\
    hor Latn TD\n\
    hot Latn PG\n\
    hov Latn ID\n\
    how Hani CN\n\
    hoy Deva IN\n\
    hpo Mymr MM\n\
    hra Latn IN\n\
    hrc Latn PG\n\
    hre Latn VN\n\
    hrk Latn ID\n\
    hrm Latn CN\n\
    hro Latn VN\n\
    hrp Latn AU\n\
    hrt Syrc TR\n\
    hru Latn IN\n\
    hrw Latn PG\n\
    hrx Latn BR\n\
    hrz Arab IR\n\
    hss Arab OM\n\
    hti Latn ID\n\
    hto Latn CO\n\
    hts Latn TZ\n\
    htu Latn ID\n\
    htx Xsux TR\n\
    hub Latn PE\n\
    huc Latn BW\n\
    hud Latn ID\n\
    hue Latn MX\n\
    huf Latn PG\n\
    hug Latn PE\n\
    huh Latn CL\n\
    hui Latn PG\n\
    huk Latn ID\n\
    hul Latn PG\n\
    hum Latn CD\n\
    hup Latn US\n\
    hus Latn MX\n\
    hut Deva NP\n\
    huu Latn PE\n\
    huv Latn MX\n\
    huw Latn ID\n\
    hux Latn PE\n\
    huy Hebr IL\n\
    huz Cyrl RU\n\
    hvc Latn HT\n\
    hve Latn MX\n\
    hvk Latn NC\n\
    hvn Latn ID\n\
    hvv Latn MX\n\
    hwa Latn CI\n\
    hwc Latn US\n\
    hwo Latn NG\n\
    hya Latn CM\n\
    hyw Armn AM\n\
    iai Latn NC\n\
    ian Latn PG\n\
    iar Latn PG\n\
    ibd Latn AU\n\
    ibe Latn NG\n\
    ibg Latn PH\n\
    ibh Latn VN\n\
    ibl Latn PH\n\
    ibm Latn NG\n\
    ibn Latn NG\n\
    ibr Latn NG\n\
    ibu Latn ID\n\
    iby Latn NG\n\
    ica Latn BJ\n\
    ich Latn NG\n\
    icr Latn CO\n\
    ida Latn KE\n\
    idb Latn IN\n\
    idc Latn NG\n\
    idd Latn BJ\n\
    ide Latn NG\n\
    idi Latn PG\n\
    idr Latn SS\n\
    ids Latn NG\n\
    idt Latn TL\n\
    idu Latn NG\n\
    ifa Latn PH\n\
    ifb Latn PH\n\
    iff Latn VU\n\
    ifk Latn PH\n\
    ifm Latn CG\n\
    ifu Latn PH\n\
    ify Latn PH\n\
    igb Latn NG\n\
    ige Latn NG\n\
    igg Latn PG\n\
    igl Latn NG\n\
    igm Latn PG\n\
    ign Latn BO\n\
    igo Latn PG\n\
    igs Latn 001\n\
    igw Latn NG\n\
    ihb Latn ID\n\
    ihi Latn NG\n\
    ihp Latn ID\n\
    ihw Latn AU\n\
    iin Latn AU\n\
    ijc Latn NG\n\
    ije Latn NG\n\
    ijj Latn BJ\n\
    ijn Latn NG\n\
    ijs Latn NG\n\
    ikh Latn NG\n\
    iki Latn NG\n\
    ikk Latn NG\n\
    ikl Latn NG\n\
    iko Latn NG\n\
    ikp Latn NG\n\
    ikr Latn AU\n\
    ikt Latn CA\n\
    ikv Latn NG\n\
    ikw Latn NG\n\
    ikx Latn UG\n\
    ikz Latn TZ\n\
    ila Latn ID\n\
    ilb Latn ZM\n\
    ilg Latn AU\n\
    ili Latn CN\n\
    ilk Latn PH\n\
    ilm Latn MY\n\
    ilp Latn PH\n\
    ilu Latn ID\n\
    ilv Latn NG\n\
    imi Latn PG\n\
    iml Latn US\n\
    imn Latn PG\n\
    imo Latn PG\n\
    imr Latn ID\n\
    ims Latn IT\n\
    imt Latn SS\n\
    imy Lyci TR\n\
    inb Latn CO\n\
    ing Latn US\n\
    inj Latn CO\n\
    inn Latn PH\n\
    ino Latn PG\n\
    inp Latn PE\n\
    int Mymr MM\n\
    ior Ethi ET\n\
    iou Latn PG\n\
    iow Latn US\n\
    ipi Latn PG\n\
    ipo Latn PG\n\
    iqu Latn PE\n\
    iqw Latn NG\n\
    ire Latn ID\n\
    irh Latn ID\n\
    iri Latn NG\n\
    irk Latn TZ\n\
    irn Latn BR\n\
    iru Taml IN\n\
    irx Latn ID\n\
    iry Latn PH\n\
    isa Latn PG\n\
    isc Latn PE\n\
    isd Latn PH\n\
    ish Latn NG\n\
    isi Latn NG\n\
    isk Arab AF\n\
    ism Latn ID\n\
    isn Latn TZ\n\
    iso Latn NG\n\
    ist Latn HR\n\
    isu Latn CM\n\
    itb Latn PH\n\
    itd Latn ID\n\
    ite Latn BO\n\
    iti Latn PH\n\
    itk Hebr IT\n\
    itl Cyrl RU\n\
    itm Latn NG\n\
    ito Latn BO\n\
    itr Latn PG\n\
    its Latn NG\n\
    itt Latn PH\n\
    itv Latn PH\n\
    itw Latn NG\n\
    itx Latn ID\n\
    ity Latn PH\n\
    itz Latn GT\n\
    ium Latn CN\n\
    ivb Latn PH\n\
    ivv Latn PH\n\
    iwk Latn PH\n\
    iwm Latn PG\n\
    iwo Latn ID\n\
    iws Latn PG\n\
    ixc Latn MX\n\
    ixl Latn GT\n\
    iya Latn NG\n\
    iyo Latn CM\n\
    iyx Latn CG\n\
    izm Latn NG\n\
    izr Latn NG\n\
    izz Latn NG\n\
    jaa Latn BR\n\
    jab Latn NG\n\
    jac Latn GT\n\
    jad Arab GN\n\
    jae Latn PG\n\
    jaf Latn NG\n\
    jah Latn MY\n\
    jaj Latn SB\n\
    jak Latn MY\n\
    jal Latn ID\n\
    jan Latn AU\n\
    jao Latn AU\n\
    jaq Latn ID\n\
    jas Latn NC\n\
    jat Arab AF\n\
    jau Latn ID\n\
    jax Latn ID\n\
    jay Latn AU\n\
    jaz Latn NC\n\
    jbe Hebr IL\n\
    jbi Latn AU\n\
    jbj Latn ID\n\
    jbk Latn PG\n\
    jbm Latn NG\n\
    jbn Arab LY\n\
    jbr Latn ID\n\
    jbt Latn BR\n\
    jbu Latn CM\n\
    jbw Latn AU\n\
    jct Cyrl UA\n\
    jda Tibt IN\n\
    jdg Arab PK\n\
    jdt Cyrl RU\n\
    jeb Latn PE\n\
    jee Deva NP\n\
    jeh Latn VN\n\
    jei Latn ID\n\
    jek Latn CI\n\
    jel Latn ID\n\
    jen Latn NG\n\
    jer Latn NG\n\
    jet Latn PG\n\
    jeu Latn TD\n\
    jgb Latn CD\n\
    jge Geor GE\n\
    jgk Latn NG\n\
    jhi Latn MY\n\
    jia Latn CM\n\
    jib Latn NG\n\
    jic Latn HN\n\
    jid Latn NG\n\
    jie Latn NG\n\
    jig Latn AU\n\
    jil Latn PG\n\
    jim Latn CM\n\
    jit Latn TZ\n\
    jiu Latn CN\n\
    jiv Latn EC\n\
    jiy Latn CN\n\
    jje Hang KR\n\
    jjr Latn NG\n\
    jka Latn ID\n\
    jkm Mymr MM\n\
    jko Latn PG\n\
    jku Latn NG\n\
    jle Latn SD\n\
    jma Latn PG\n\
    jmb Latn NG\n\
    jmd Latn ID\n\
    jmi Latn NG\n\
    jmn Latn MM\n\
    jmr Latn GH\n\
    jms Latn NG\n\
    jmw Latn PG\n\
    jmx Latn MX\n\
    jna Takr IN\n\
    jnd Arab PK\n\
    jng Latn AU\n\
    jni Latn NG\n\
    jnj Latn ET\n\
    jnl Deva IN\n\
    jns Deva IN\n\
    job Latn CD\n\
    jod Latn CI\n\
    jog Arab PK\n\
    jor Latn BO\n\
    jow Latn ML\n\
    jpa Hebr PS\n\
    jpr Hebr IL\n\
    jqr Latn PE\n\
    jra Latn VN\n\
    jrb Hebr IL\n\
    jrr Latn NG\n\
    jrt Latn NG\n\
    jru Latn VE\n\
    jua Latn BR\n\
    jub Latn NG\n\
    jud Latn CI\n\
    juh Latn NG\n\
    jui Latn AU\n\
    juk Latn NG\n\
    jul Deva NP\n\
    jum Latn SD\n\
    jun Orya IN\n\
    juo Latn NG\n\
    jup Latn BR\n\
    jur Latn BR\n\
    juu Latn NG\n\
    juw Latn NG\n\
    juy Orya IN\n\
    jvd Latn ID\n\
    jvn Latn SR\n\
    jwi Latn GH\n\
    jya Tibt CN\n\
    jye Hebr IL\n\
    jyy Latn TD\n\
    kad Latn NG\n\
    kag Latn MY\n\
    kah Latn CF\n\
    kai Latn NG\n\
    kak Latn PH\n\
    kap Cyrl RU\n\
    kaq Latn PE\n\
    kav Latn BR\n\
    kax Latn ID\n\
    kay Latn BR\n\
    kba Latn AU\n\
    kbb Latn BR\n\
    kbc Latn BR\n\
    kbe Latn AU\n\
    kbg Tibt IN\n\
    kbh Latn CO\n\
    kbi Latn ID\n\
    kbj Latn CD\n\
    kbk Latn PG\n\
    kbl Latn TD\n\
    kbm Latn PG\n\
    kbn Latn CF\n\
    kbo Latn SS\n\
    kbp Latn TG\n\
    kbq Latn PG\n\
    kbr Latn ET\n\
    kbs Latn GA\n\
    kbt Latn PG\n\
    kbu Arab PK\n\
    kbv Latn ID\n\
    kbw Latn PG\n\
    kbx Latn PG\n\
    kbz Latn NG\n\
    kca Cyrl RU\n\
    kcb Latn PG\n\
    kcc Latn NG\n\
    kcd Latn ID\n\
    kce Latn NG\n\
    kcf Latn NG\n\
    kch Latn NG\n\
    kci Latn NG\n\
    kcj Latn GW\n\
    kcl Latn PG\n\
    kcm Latn CF\n\
    kcn Latn UG\n\
    kco Latn PG\n\
    kcp Latn SD\n\
    kcq Latn NG\n\
    kcs Latn NG\n\
    kct Latn PG\n\
    kcu Latn TZ\n\
    kcv Latn CD\n\
    kcw Latn CD\n\
    kcy Arab DZ\n\
    kcz Latn TZ\n\
    kda Latn AU\n\
    kdc Latn TZ\n\
    kdd Latn AU\n\
    kdf Latn PG\n\
    kdg Latn CD\n\
    kdi Latn UG\n\
    kdj Latn UG\n\
    kdk Latn NC\n\
    kdl Latn NG\n\
    kdm Latn NG\n\
    kdn Latn ZW\n\
    kdp Latn NG\n\
    kdq Beng IN\n\
    kdr Latn LT\n\
    kdw Latn ID\n\
    kdx Latn NG\n\
    kdy Latn ID\n\
    kdz Latn CM\n\
    keb Latn GA\n\
    kec Latn SD\n\
    ked Latn TZ\n\
    kee Latn US\n\
    kef Latn TG\n\
    keg Latn SD\n\
    keh Latn PG\n\
    kei Latn ID\n\
    kek Latn GT\n\
    kel Latn CD\n\
    kem Latn TL\n\
    keo Latn UG\n\
    ker Latn TD\n\
    kes Latn NG\n\
    ket Cyrl RU\n\
    keu Latn TG\n\
    kev Mlym IN\n\
    kew Latn PG\n\
    kex Deva IN\n\
    key Telu IN\n\
    kez Latn NG\n\
    kfa Knda IN\n\
    kfb Deva IN\n\
    kfc Telu IN\n\
    kfd Knda IN\n\
    kfe Taml IN\n\
    kff Latn IN\n\
    kfg Knda IN\n\
    kfh Mlym IN\n\
    kfi Taml IN\n\
    kfk Deva IN\n\
    kfl Latn CM\n\
    kfm Arab IR\n\
    kfn Latn CM\n\
    kfp Deva IN\n\
    kfq Deva IN\n\
    kfs Deva IN\n\
    kfu Deva IN\n\
    kfv Latn IN\n\
    kfw Latn IN\n\
    kfx Deva IN\n\
    kfz Latn BF\n\
    kga Latn CI\n\
    kgb Latn ID\n\
    kgf Latn PG\n\
    kgj Deva NP\n\
    kgk Latn BR\n\
    kgl Latn AU\n\
    kgo Latn SD\n\
    kgq Latn ID\n\
    kgr Latn ID\n\
    kgs Latn AU\n\
    kgt Latn NG\n\
    kgu Latn PG\n\
    kgv Latn ID\n\
    kgw Latn ID\n\
    kgx Latn ID\n\
    kgy Deva NP\n\
    khc Latn ID\n\
    khd Latn ID\n\
    khe Latn ID\n\
    khf Thai LA\n\
    khg Tibt CN\n\
    khh Latn ID\n\
    khj Latn NG\n\
    khl Latn PG\n\
    kho Brah IR\n\
    khp Latn ID\n\
    khr Latn IN\n\
    khs Latn PG\n\
    khu Latn AO\n\
    khv Cyrl RU\n\
    khx Latn CD\n\
    khy Latn CD\n\
    khz Latn PG\n\
    kia Latn TD\n\
    kib Latn SD\n\
    kic Latn US\n\
    kid Latn CM\n\
    kie Latn TD\n\
    kif Deva NP\n\
    kig Latn ID\n\
    kih Latn PG\n\
    kij Latn PG\n\
    kil Latn NG\n\
    kim Cyrl RU\n\
    kio Latn US\n\
    kip Deva NP\n\
    kiq Latn ID\n\
    kis Latn PG\n\
    kit Latn PG\n\
    kiv Latn TZ\n\
    kiw Latn PG\n\
    kix Latn IN\n\
    kiy Latn ID\n\
    kiz Latn TZ\n\
    kja Latn ID\n\
    kjb Latn GT\n\
    kjc Latn ID\n\
    kjd Latn PG\n\
    kje Latn ID\n\
    kjh Cyrl RU\n\
    kji Latn SB\n\
    kjj Latn AZ\n\
    kjk Latn ID\n\
    kjl Deva NP\n\
    kjm Latn VN\n\
    kjn Latn AU\n\
    kjo Deva IN\n\
    kjp Mymr MM\n\
    kjq Latn US\n\
    kjr Latn ID\n\
    kjs Latn PG\n\
    kjt Thai TH\n\
    kju Latn US\n\
    kjx Latn PG\n\
    kjy Latn PG\n\
    kjz Tibt BT\n\
    kka Latn NG\n\
    kkb Latn ID\n\
    kkc Latn PG\n\
    kkd Latn NG\n\
    kke Latn GN\n\
    kkf Tibt IN\n\
    kkg Latn PH\n\
    kkh Lana MM\n\
    kki Latn TZ\n\
    kkk Latn SB\n\
    kkl Latn ID\n\
    kkm Latn NG\n\
    kko Latn SD\n\
    kkp Latn AU\n\
    kkq Latn CD\n\
    kkr Latn NG\n\
    kks Latn NG\n\
    kkt Deva NP\n\
    kku Latn NG\n\
    kkv Latn ID\n\
    kkw Latn CG\n\
    kkx Latn ID\n\
    kky Latn AU\n\
    kkz Latn CA\n\
    kla Latn US\n\
    klb Latn MX\n\
    klc Latn CM\n\
    kld Latn AU\n\
    kle Deva NP\n\
    klf Latn TD\n\
    klg Latn PH\n\
    klh Latn PG\n\
    kli Latn ID\n\
    klj Arab IR\n\
    klk Latn NG\n\
    kll Latn PH\n\
    klm Latn PG\n\
    klo Latn NG\n\
    klp Latn PG\n\
    klq Latn PG\n\
    klr Deva NP\n\
    kls Latn PK\n\
    klt Latn PG\n\
    klu Latn LR\n\
    klv Latn VU\n\
    klw Latn ID\n\
    klx Latn PG\n\
    kly Latn ID\n\
    klz Latn ID\n\
    kma Latn GH\n\
    kmc Latn CN\n\
    kmd Latn PH\n\
    kme Latn CM\n\
    kmf Latn PG\n\
    kmg Latn PG\n\
    kmh Latn PG\n\
    kmi Latn NG\n\
    kmj Deva IN\n\
    kmk Latn PH\n\
    kml Latn PH\n\
    kmm Latn IN\n\
    kmn Latn PG\n\
    kmo Latn PG\n\
    kmp Latn CM\n\
    kmq Latn ET\n\
    kms Latn PG\n\
    kmt Latn ID\n\
    kmu Latn PG\n\
    kmv Latn BR\n\
    kmw Latn CD\n\
    kmx Latn PG\n\
    kmy Latn NG\n\
    kmz Arab IR\n\
    kna Latn NG\n\
    knb Latn PH\n\
    knd Latn ID\n\
    kne Latn PH\n\
    kni Latn NG\n\
    knj Latn GT\n\
    knk Latn SL\n\
    knl Latn ID\n\
    knm Latn BR\n\
    kno Latn SL\n\
    knp Latn CM\n\
    knq Latn MY\n\
    knr Latn PG\n\
    kns Latn MY\n\
    knt Latn BR\n\
    knu Latn GN\n\
    knv Latn PG\n\
    knw Latn NA\n\
    knx Latn ID\n\
    kny Latn CD\n\
    knz Latn BF\n\
    koa Latn PG\n\
    koc Latn NG\n\
    kod Latn ID\n\
    koe Latn SS\n\
    kof Latn NG\n\
    kog Latn CO\n\
    koh Latn CG\n\
    kol Latn PG\n\
    koo Latn UG\n\
    kop Latn PG\n\
    koq Latn GA\n\
    kot Latn CM\n\
    kou Latn TD\n\
    kov Latn NG\n\
    kow Latn NG\n\
    koy Latn US\n\
    koz Latn PG\n\
    kpa Latn NG\n\
    kpc Latn CO\n\
    kpd Latn ID\n\
    kpf Latn PG\n\
    kpg Latn FM\n\
    kph Latn GH\n\
    kpi Latn ID\n\
    kpj Latn BR\n\
    kpk Latn NG\n\
    kpl Latn CD\n\
    kpm Latn VN\n\
    kpn Latn BR\n\
    kpo Latn TG\n\
    kpq Latn ID\n\
    kpr Latn PG\n\
    kps Latn ID\n\
    kpt Cyrl RU\n\
    kpu Latn ID\n\
    kpw Latn PG\n\
    kpx Latn PG\n\
    kpy Cyrl RU\n\
    kpz Latn UG\n\
    kqa Latn PG\n\
    kqb Latn PG\n\
    kqc Latn PG\n\
    kqd Syrc IQ\n\
    kqe Latn PH\n\
    kqf Latn PG\n\
    kqg Latn BF\n\
    kqh Latn TZ\n\
    kqi Latn PG\n\
    kqj Latn PG\n\
    kqk Latn BJ\n\
    kql Latn PG\n\
    kqm Latn CI\n\
    kqo Latn LR\n\
    kqp Latn TD\n\
    kqq Latn BR\n\
    kqr Latn MY\n\
    kqs Latn GN\n\
    kqt Latn MY\n\
    kqu Latn ZA\n\
    kqv Latn ID\n\
    kqw Latn PG\n\
    kqx Latn CM\n\
    kqy Ethi ET\n\
    kqz Latn ZA\n\
    kr Latn NG\n\
    kra Deva NP\n\
    krb Latn US\n\
    krd Latn TL\n\
    kre Latn BR\n\
    krf Latn VU\n\
    krh Latn NG\n\
    krk Cyrl RU\n\
    krn Latn LR\n\
    krp Latn NG\n\
    krr Khmr KH\n\
    krs Latn SS\n\
    krt Latn NE\n\
    krv Khmr KH\n\
    krw Latn LR\n\
    krx Latn SN\n\
    kry Latn AZ\n\
    krz Latn ID\n\
    ksc Latn PH\n\
    ksd Latn PG\n\
    kse Latn PG\n\
    ksg Latn SB\n\
    ksi Latn PG\n\
    ksj Latn PG\n\
    ksk Latn US\n\
    ksl Latn PG\n\
    ksm Latn NG\n\
    ksn Latn PH\n\
    kso Latn NG\n\
    ksp Latn CF\n\
    ksq Latn NG\n\
    ksr Latn PG\n\
    kss Latn LR\n\
    kst Latn BF\n\
    ksu Mymr IN\n\
    ksv Latn CD\n\
    ksw Mymr MM\n\
    ksx Latn ID\n\
    ksz Deva IN\n\
    kta Latn VN\n\
    ktb Ethi ET\n\
    ktc Latn NG\n\
    ktd Latn AU\n\
    kte Deva NP\n\
    ktf Latn CD\n\
    ktg Latn AU\n\
    kth Latn TD\n\
    kti Latn ID\n\
    ktj Latn CI\n\
    ktk Latn PG\n\
    ktl Arab IR\n\
    ktm Latn PG\n\
    ktn Latn BR\n\
    kto Latn PG\n\
    ktp Plrd CN\n\
    ktq Latn PH\n\
    kts Latn ID\n\
    ktt Latn ID\n\
    ktu Latn CD\n\
    ktv Latn VN\n\
    ktw Latn US\n\
    ktx Latn BR\n\
    kty Latn CD\n\
    ktz Latn NA\n\
    kub Latn NG\n\
    kuc Latn ID\n\
    kud Latn PG\n\
    kue Latn PG\n\
    kuf Laoo LA\n\
    kug Latn NG\n\
    kuh Latn NG\n\
    kui Latn BR\n\
    kuj Latn TZ\n\
    kuk Latn ID\n\
    kul Latn NG\n\
    kun Latn ER\n\
    kuo Latn PG\n\
    kup Latn PG\n\
    kuq Latn BR\n\
    kus Latn GH\n\
    kut Latn CA\n\
    kuu Latn US\n\
    kuv Latn ID\n\
    kuw Latn CF\n\
    kux Latn AU\n\
    kuy Latn AU\n\
    kuz Latn CL\n\
    kva Cyrl RU\n\
    kvb Latn ID\n\
    kvc Latn PG\n\
    kvd Latn ID\n\
    kve Latn MY\n\
    kvf Latn TD\n\
    kvg Latn PG\n\
    kvh Latn ID\n\
    kvi Latn TD\n\
    kvj Latn CM\n\
    kvl Latn MM\n\
    kvm Latn CM\n\
    kvn Latn CO\n\
    kvo Latn ID\n\
    kvp Latn ID\n\
    kvq Mymr MM\n\
    kvt Mymr MM\n\
    kvv Latn ID\n\
    kvw Latn ID\n\
    kvy Kali MM\n\
    kvz Latn ID\n\
    kwa Latn BR\n\
    kwb Latn NG\n\
    kwc Latn CG\n\
    kwd Latn SB\n\
    kwe Latn ID\n\
    kwf Latn SB\n\
    kwg Latn TD\n\
    kwh Latn ID\n\
    kwi Latn CO\n\
    kwj Latn PG\n\
    kwl Latn NG\n\
    kwm Latn NA\n\
    kwn Latn NA\n\
    kwo Latn PG\n\
    kwp Latn CI\n\
    kwr Latn ID\n\
    kws Latn CD\n\
    kwt Latn ID\n\
    kwu Latn CM\n\
    kwv Latn TD\n\
    kww Latn SR\n\
    kwy Latn AO\n\
    kwz Latn AO\n\
    kxa Latn PG\n\
    kxb Latn CI\n\
    kxc Latn ET\n\
    kxd Latn BN\n\
    kxf Mymr MM\n\
    kxi Latn MY\n\
    kxj Latn TD\n\
    kxk Mymr MM\n\
    kxn Latn MY\n\
    kxo Latn BR\n\
    kxq Latn ID\n\
    kxr Latn PG\n\
    kxt Latn PG\n\
    kxw Latn PG\n\
    kxx Latn CG\n\
    kxy Latn VN\n\
    kxz Latn PG\n\
    kya Latn TZ\n\
    kyb Latn PH\n\
    kyc Latn PG\n\
    kyd Latn ID\n\
    kye Latn GH\n\
    kyf Latn CI\n\
    kyg Latn PG\n\
    kyh Latn US\n\
    kyi Latn MY\n\
    kyj Latn PH\n\
    kyk Latn PH\n\
    kyl Latn US\n\
    kym Latn CF\n\
    kyn Latn PH\n\
    kyo Latn ID\n\
    kyq Latn TD\n\
    kyr Latn BR\n\
    kys Latn MY\n\
    kyt Latn ID\n\
    kyu Kali MM\n\
    kyv Deva NP\n\
    kyw Deva IN\n\
    kyx Latn PG\n\
    kyy Latn PG\n\
    kyz Latn BR\n\
    kza Latn BF\n\
    kzb Latn ID\n\
    kzc Latn CI\n\
    kzd Latn ID\n\
    kze Latn PG\n\
    kzf Latn ID\n\
    kzi Latn MY\n\
    kzk Latn SB\n\
    kzl Latn ID\n\
    kzm Latn ID\n\
    kzn Latn MW\n\
    kzo Latn GA\n\
    kzp Latn ID\n\
    kzr Latn CM\n\
    kzs Latn MY\n\
    kzu Latn ID\n\
    kzv Latn ID\n\
    kzw Latn BR\n\
    kzx Latn ID\n\
    kzy Latn CD\n\
    kzz Latn ID\n\
    laa Latn PH\n\
    lac Latn MX\n\
    lae Deva IN\n\
    lai Latn MW\n\
    lal Latn CD\n\
    lam Latn ZM\n\
    lan Latn NG\n\
    lap Latn TD\n\
    laq Latn VN\n\
    lar Latn GH\n\
    las Latn TG\n\
    lau Latn ID\n\
    law Latn ID\n\
    lax Latn IN\n\
    laz Latn PG\n\
    lbb Latn PG\n\
    lbf Deva IN\n\
    lbi Latn CM\n\
    lbj Tibt IN\n\
    lbl Latn PH\n\
    lbm Deva IN\n\
    lbn Latn LA\n\
    lbo Laoo LA\n\
    lbq Latn PG\n\
    lbr Deva NP\n\
    lbt Latn VN\n\
    lbu Latn PG\n\
    lbv Latn PG\n\
    lbx Latn ID\n\
    lby Latn AU\n\
    lbz Latn AU\n\
    lcc Latn ID\n\
    lcd Latn ID\n\
    lce Latn ID\n\
    lcf Latn ID\n\
    lch Latn AO\n\
    lcl Latn ID\n\
    lcm Latn PG\n\
    lcq Latn ID\n\
    lcs Latn ID\n\
    lda Latn CI\n\
    ldb Latn NG\n\
    ldd Latn NG\n\
    ldg Latn NG\n\
    ldh Latn NG\n\
    ldi Latn CG\n\
    ldj Latn NG\n\
    ldk Latn NG\n\
    ldl Latn NG\n\
    ldm Latn GN\n\
    ldn Latn 001\n\
    ldo Latn NG\n\
    ldp Latn NG\n\
    ldq Latn NG\n\
    lea Latn CD\n\
    lec Latn BO\n\
    led Latn CD\n\
    lee Latn BF\n\
    lef Latn GH\n\
    leh Latn ZM\n\
    lei Latn PG\n\
    lej Latn CD\n\
    lek Latn PG\n\
    lel Latn CD\n\
    lem Latn CM\n\
    leo Latn CM\n\
    leq Latn PG\n\
    ler Latn PG\n\
    les Latn CD\n\
    let Latn PG\n\
    leu Latn PG\n\
    lev Latn ID\n\
    lew Latn ID\n\
    lex Latn ID\n\
    ley Latn ID\n\
    lfa Latn CM\n\
    lfn Latn 001\n\
    lga Latn SB\n\
    lgb Latn SB\n\
    lgg Latn UG\n\
    lgh Latn VN\n\
    lgi Latn ID\n\
    lgk Latn VU\n\
    lgl Latn SB\n\
    lgm Latn CD\n\
    lgn Latn ET\n\
    lgo Latn SS\n\
    lgq Latn GH\n\
    lgr Latn SB\n\
    lgt Latn PG\n\
    lgu Latn SB\n\
    lgz Latn CD\n\
    lha Latn VN\n\
    lhh Latn ID\n\
    lhi Latn CN\n\
    lhm Deva NP\n\
    lhn Latn MY\n\
    lhs Syrc SY\n\
    lht Latn VU\n\
    lhu Latn CN\n\
    lia Latn SL\n\
    lib Latn PG\n\
    lic Latn CN\n\
    lid Latn PG\n\
    lie Latn CD\n\
    lig Latn GH\n\
    lih Latn PG\n\
    lik Latn CD\n\
    lio Latn ID\n\
    lip Latn GH\n\
    liq Latn ET\n\
    lir Latn LR\n\
    liu Latn SD\n\
    liv Latn LV\n\
    liw Latn ID\n\
    lix Latn ID\n\
    liy Latn CF\n\
    liz Latn CD\n\
    lja Latn AU\n\
    lje Latn ID\n\
    lji Latn ID\n\
    ljl Latn ID\n\
    ljw Latn AU\n\
    ljx Latn AU\n\
    lka Latn TL\n\
    lkb Latn KE\n\
    lkc Latn VN\n\
    lkd Latn BR\n\
    lke Latn UG\n\
    lkh Tibt BT\n\
    lkj Latn MY\n\
    lkl Latn PG\n\
    lkm Latn AU\n\
    lkn Latn VU\n\
    lko Latn KE\n\
    lkr Latn SS\n\
    lks Latn KE\n\
    lku Latn AU\n\
    lky Latn SS\n\
    lla Latn NG\n\
    llb Latn MZ\n\
    llc Latn GN\n\
    lle Latn PG\n\
    llf Latn PG\n\
    llg Latn ID\n\
    lli Latn CG\n\
    llj Latn AU\n\
    llk Latn MY\n\
    lll Latn PG\n\
    llm Latn ID\n\
    lln Latn TD\n\
    llp Latn VU\n\
    llq Latn ID\n\
    llu Latn SB\n\
    llx Latn FJ\n\
    lma Latn GN\n\
    lmb Latn VU\n\
    lmc Latn AU\n\
    lmd Latn SD\n\
    lme Latn TD\n\
    lmf Latn ID\n\
    lmg Latn PG\n\
    lmh Deva NP\n\
    lmi Latn CD\n\
    lmj Latn ID\n\
    lmk Latn IN\n\
    lml Latn VU\n\
    lmp Latn CM\n\
    lmq Latn ID\n\
    lmr Latn ID\n\
    lmu Latn VU\n\
    lmv Latn FJ\n\
    lmw Latn US\n\
    lmx Latn CM\n\
    lmy Latn ID\n\
    lna Latn CF\n\
    lnb Latn NA\n\
    lnd Latn ID\n\
    lng Latn HU\n\
    lnh Latn MY\n\
    lni Latn PG\n\
    lnj Latn AU\n\
    lnl Latn CF\n\
    lnm Latn PG\n\
    lnn Latn VU\n\
    lns Latn CM\n\
    lnu Latn NG\n\
    lnw Latn AU\n\
    lnz Latn CD\n\
    loa Latn ID\n\
    lob Latn BF\n\
    loc Latn PH\n\
    loe Latn ID\n\
    log Latn CD\n\
    loh Latn SS\n\
    loi Latn CI\n\
    loj Latn PG\n\
    lok Latn SL\n\
    lom Latn LR\n\
    lon Latn MW\n\
    loo Latn CD\n\
    lop Latn NG\n\
    loq Latn CD\n\
    lor Latn CI\n\
    los Latn PG\n\
    lot Latn SS\n\
    lou Latn US\n\
    low Latn MY\n\
    lox Latn ID\n\
    loy Deva NP\n\
    lpa Latn VU\n\
    lpe Latn ID\n\
    lpn Latn MM\n\
    lpo Plrd CN\n\
    lpx Latn SS\n\
    lqr Latn SS\n\
    lra Latn MY\n\
    lrg Latn AU\n\
    lri Latn KE\n\
    lrk Arab PK\n\
    lrl Arab IR\n\
    lrm Latn KE\n\
    lrn Latn ID\n\
    lro Latn SD\n\
    lrt Latn ID\n\
    lrv Latn VU\n\
    lrz Latn VU\n\
    lsa Arab IR\n\
    lsd Hebr IL\n\
    lse Latn CD\n\
    lsi Latn MM\n\
    lsm Latn UG\n\
    lsr Latn PG\n\
    lss Arab PK\n\
    ltc Hant CN\n\
    lth Latn UG\n\
    lti Latn ID\n\
    ltn Latn BR\n\
    lto Latn KE\n\
    lts Latn KE\n\
    ltu Latn ID\n\
    luc Latn UG\n\
    lud Latn RU\n\
    luf Latn PG\n\
    lui Latn US\n\
    luj Latn CD\n\
    luk Tibt BT\n\
    lul Latn SS\n\
    lum Latn AO\n\
    lup Latn GA\n\
    luq Latn CU\n\
    lur Latn ID\n\
    lus Latn IN\n\
    lut Latn US\n\
    luu Deva NP\n\
    luv Arab OM\n\
    luw Latn CM\n\
    lva Latn TL\n\
    lvi Latn LA\n\
    lvk Latn SB\n\
    lvl Latn CD\n\
    lvu Latn ID\n\
    lwa Latn CD\n\
    lwe Latn ID\n\
    lwg Latn KE\n\
    lwh Latn VN\n\
    lwm Thai CN\n\
    lwo Latn SS\n\
    lwt Latn ID\n\
    lww Latn VU\n\
    lxm Latn PG\n\
    lya Tibt BT\n\
    lyn Latn ZM\n\
    lzl Latn VU\n\
    lzn Latn MM\n\
    maa Latn MX\n\
    mab Latn MX\n\
    mae Latn NG\n\
    maj Latn MX\n\
    mam Latn GT\n\
    maq Latn MX\n\
    mat Latn MX\n\
    mau Latn MX\n\
    mav Latn BR\n\
    maw Latn GH\n\
    max Latn ID\n\
    mba Latn PH\n\
    mbb Latn PH\n\
    mbc Latn BR\n\
    mbd Latn PH\n\
    mbf Latn SG\n\
    mbh Latn PG\n\
    mbi Latn PH\n\
    mbj Latn BR\n\
    mbk Latn PG\n\
    mbl Latn BR\n\
    mbm Latn CG\n\
    mbn Latn CO\n\
    mbo Latn CM\n\
    mbp Latn CO\n\
    mbq Latn PG\n\
    mbr Latn CO\n\
    mbs Latn PH\n\
    mbt Latn PH\n\
    mbu Latn NG\n\
    mbv Latn GN\n\
    mbw Latn PG\n\
    mbx Latn PG\n\
    mby Arab PK\n\
    mbz Latn MX\n\
    mca Latn PY\n\
    mcb Latn PE\n\
    mcc Latn PG\n\
    mcd Latn PE\n\
    mce Latn MX\n\
    mcf Latn PE\n\
    mcg Latn VE\n\
    mch Latn VE\n\
    mci Latn PG\n\
    mcj Latn NG\n\
    mck Latn AO\n\
    mcl Latn CO\n\
    mcm Latn MY\n\
    mcn Latn TD\n\
    mco Latn MX\n\
    mcp Latn CM\n\
    mcq Latn PG\n\
    mcr Latn PG\n\
    mcs Latn CM\n\
    mct Latn CM\n\
    mcu Latn CM\n\
    mcv Latn PG\n\
    mcw Latn TD\n\
    mcx Latn CF\n\
    mcy Latn PG\n\
    mcz Latn PG\n\
    mda Latn NG\n\
    mdb Latn PG\n\
    mdc Latn PG\n\
    mdd Latn CM\n\
    mde Arab TD\n\
    mdg Latn TD\n\
    mdi Latn CD\n\
    mdj Latn CD\n\
    mdk Latn CD\n\
    mdm Latn CD\n\
    mdn Latn CF\n\
    mdp Latn CD\n\
    mdq Latn CD\n\
    mds Latn PG\n\
    mdt Latn CG\n\
    mdu Latn CG\n\
    mdv Latn MX\n\
    mdw Latn CG\n\
    mdx Ethi ET\n\
    mdy Ethi ET\n\
    mdz Latn BR\n\
    mea Latn CM\n\
    meb Latn PG\n\
    mec Latn AU\n\
    med Latn PG\n\
    mee Latn PG\n\
    meh Latn MX\n\
    mej Latn ID\n\
    mek Latn PG\n\
    mel Latn MY\n\
    mem Latn AU\n\
    meo Latn MY\n\
    mep Latn AU\n\
    meq Latn CM\n\
    mes Latn TD\n\
    met Latn PG\n\
    meu Latn PG\n\
    mev Latn LR\n\
    mew Latn NG\n\
    mez Latn US\n\
    mfb Latn ID\n\
    mfc Latn CD\n\
    mfd Latn CM\n\
    mff Latn CM\n\
    mfg Latn GN\n\
    mfh Latn CM\n\
    mfi Arab CM\n\
    mfj Latn CM\n\
    mfk Latn CM\n\
    mfl Latn NG\n\
    mfm Latn NG\n\
    mfn Latn NG\n\
    mfo Latn NG\n\
    mfp Latn ID\n\
    mfq Latn TG\n\
    mfr Latn AU\n\
    mft Latn PG\n\
    mfu Latn AO\n\
    mfw Latn PG\n\
    mfx Latn ET\n\
    mfy Latn MX\n\
    mfz Latn SS\n\
    mga Latg IE\n\
    mgb Latn TD\n\
    mgc Latn SS\n\
    mgd Latn SS\n\
    mge Latn TD\n\
    mgf Latn ID\n\
    mgg Latn CM\n\
    mgi Latn NG\n\
    mgj Latn NG\n\
    mgk Latn ID\n\
    mgl Latn PG\n\
    mgm Latn TL\n\
    mgn Latn CF\n\
    mgq Latn TZ\n\
    mgr Latn ZM\n\
    mgs Latn TZ\n\
    mgt Latn PG\n\
    mgu Latn PG\n\
    mgv Latn TZ\n\
    mgw Latn TZ\n\
    mgz Latn TZ\n\
    mhb Latn GA\n\
    mhc Latn MX\n\
    mhd Latn TZ\n\
    mhe Latn MY\n\
    mhf Latn PG\n\
    mhg Latn AU\n\
    mhi Latn UG\n\
    mhj Arab AF\n\
    mhk Latn CM\n\
    mhl Latn PG\n\
    mhm Latn MZ\n\
    mho Latn ZM\n\
    mhp Latn ID\n\
    mhq Latn US\n\
    mhs Latn ID\n\
    mht Latn VE\n\
    mhu Latn IN\n\
    mhw Latn BW\n\
    mhx Latn MM\n\
    mhy Latn ID\n\
    mhz Latn ID\n\
    mia Latn US\n\
    mib Latn MX\n\
    mid Mand IQ\n\
    mie Latn MX\n\
    mif Latn CM\n\
    mig Latn MX\n\
    mih Latn MX\n\
    mii Latn MX\n\
    mij Latn CM\n\
    mik Latn US\n\
    mil Latn MX\n\
    mim Latn MX\n\
    mio Latn MX\n\
    mip Latn MX\n\
    miq Latn NI\n\
    mir Latn MX\n\
    mit Latn MX\n\
    miu Latn MX\n\
    miw Latn PG\n\
    mix Latn MX\n\
    miy Latn MX\n\
    miz Latn MX\n\
    mjb Latn TL\n\
    mjc Latn MX\n\
    mjd Latn US\n\
    mje Latn TD\n\
    mjg Latn CN\n\
    mjh Latn TZ\n\
    mji Latn CN\n\
    mjj Latn PG\n\
    mjk Latn PG\n\
    mjl Deva IN\n\
    mjm Latn PG\n\
    mjn Latn PG\n\
    mjq Mlym IN\n\
    mjr Mlym IN\n\
    mjs Latn NG\n\
    mjt Deva IN\n\
    mju Telu IN\n\
    mjv Mlym IN\n\
    mjw Latn IN\n\
    mjx Latn BD\n\
    mjy Latn US\n\
    mjz Deva NP\n\
    mka Latn CI\n\
    mkb Deva IN\n\
    mkc Latn PG\n\
    mke Deva IN\n\
    mkf Latn NG\n\
    mki Arab PK\n\
    mkj Latn FM\n\
    mkk Latn CM\n\
    mkl Latn BJ\n\
    mkm Thai TH\n\
    mkn Latn ID\n\
    mko Latn NG\n\
    mkp Latn PG\n\
    mkr Latn PG\n\
    mks Latn MX\n\
    mkt Latn NC\n\
    mku Latn GN\n\
    mkv Latn VU\n\
    mkw Latn CG\n\
    mkx Latn PH\n\
    mky Latn ID\n\
    mkz Latn TL\n\
    mla Latn VU\n\
    mlb Latn CM\n\
    mlc Latn VN\n\
    mle Latn PG\n\
    mlf Thai LA\n\
    mlh Latn PG\n\
    mli Latn ID\n\
    mlj Latn TD\n\
    mlk Latn KE\n\
    mll Latn VU\n\
    mln Latn SB\n\
    mlo Latn SN\n\
    mlp Latn PG\n\
    mlq Latn SN\n\
    mlr Latn CM\n\
    mlu Latn SB\n\
    mlv Latn VU\n\
    mlw Latn CM\n\
    mlx Latn VU\n\
    mlz Latn PH\n\
    mma Latn NG\n\
    mmb Latn ID\n\
    mmc Latn MX\n\
    mmd Latn CN\n\
    mme Latn VU\n\
    mmf Latn NG\n\
    mmg Latn VU\n\
    mmh Latn BR\n\
    mmi Latn PG\n\
    mmm Latn VU\n\
    mmn Latn PH\n\
    mmo Latn PG\n\
    mmp Latn PG\n\
    mmq Latn PG\n\
    mmr Latn CN\n\
    mmt Latn PG\n\
    mmu Latn CM\n\
    mmv Latn BR\n\
    mmw Latn VU\n\
    mmx Latn PG\n\
    mmy Latn TD\n\
    mmz Latn CD\n\
    mna Latn PG\n\
    mnb Latn ID\n\
    mnc Mong CN\n\
    mnd Latn BR\n\
    mne Latn TD\n\
    mnf Latn CM\n\
    mng Latn VN\n\
    mnh Latn CD\n\
    mnj Arab AF\n\
    mnl Latn VU\n\
    mnm Latn PG\n\
    mnn Latn VN\n\
    mnp Latn CN\n\
    mnq Latn MY\n\
    mnr Latn US\n\
    mns Cyrl RU\n\
    mnu Latn ID\n\
    mnv Latn SB\n\
    mnx Latn ID\n\
    mny Latn MZ\n\
    mnz Latn ID\n\
    moa Latn CI\n\
    moc Latn AR\n\
    mod Latn US\n\
    mog Latn ID\n\
    moi Latn NG\n\
    moj Latn CG\n\
    mok Latn ID\n\
    mom Latn NI\n\
    moo Latn VN\n\
    mop Latn BZ\n\
    moq Latn ID\n\
    mor Latn SD\n\
    mot Latn CO\n\
    mou Latn TD\n\
    mov Latn US\n\
    mow Latn CG\n\
    mox Latn PG\n\
    moy Latn ET\n\
    moz Latn TD\n\
    mpa Latn TZ\n\
    mpb Latn AU\n\
    mpc Latn AU\n\
    mpd Latn BR\n\
    mpe Latn ET\n\
    mpg Latn TD\n\
    mph Latn AU\n\
    mpi Latn CM\n\
    mpj Latn AU\n\
    mpk Latn TD\n\
    mpl Latn PG\n\
    mpm Latn MX\n\
    mpn Latn PG\n\
    mpo Latn PG\n\
    mpp Latn PG\n\
    mpq Latn BR\n\
    mpr Latn SB\n\
    mps Latn PG\n\
    mpt Latn PG\n\
    mpu Latn BR\n\
    mpv Latn PG\n\
    mpw Latn BR\n\
    mpx Latn PG\n\
    mpy Latn ID\n\
    mpz Thai TH\n\
    mqa Latn ID\n\
    mqb Latn CM\n\
    mqc Latn ID\n\
    mqe Latn PG\n\
    mqf Latn ID\n\
    mqg Latn ID\n\
    mqh Latn MX\n\
    mqi Latn ID\n\
    mqj Latn ID\n\
    mqk Latn PH\n\
    mql Latn BJ\n\
    mqm Latn PF\n\
    mqn Latn ID\n\
    mqo Latn ID\n\
    mqp Latn ID\n\
    mqq Latn MY\n\
    mqr Latn ID\n\
    mqs Latn ID\n\
    mqu Latn SS\n\
    mqv Latn PG\n\
    mqw Latn PG\n\
    mqx Latn ID\n\
    mqy Latn ID\n\
    mqz Latn PG\n\
    mra Thai TH\n\
    mrb Latn VU\n\
    mrc Latn US\n\
    mrf Latn ID\n\
    mrg Latn IN\n\
    mrh Latn IN\n\
    mrk Latn NC\n\
    mrl Latn FM\n\
    mrm Latn VU\n\
    mrn Latn SB\n\
    mrp Latn VU\n\
    mrq Latn PF\n\
    mrr Deva IN\n\
    mrs Latn VU\n\
    mrt Latn NG\n\
    mru Latn CM\n\
    mrv Latn PF\n\
    mrw Latn PH\n\
    mrx Latn ID\n\
    mry Latn PH\n\
    mrz Latn ID\n\
    msb Latn PH\n\
    msc Latn GN\n\
    mse Latn TD\n\
    msf Latn ID\n\
    msg Latn ID\n\
    msh Latn MG\n\
    msi Latn MY\n\
    msj Latn CD\n\
    msk Latn PH\n\
    msl Latn ID\n\
    msm Latn PH\n\
    msn Latn VU\n\
    mso Latn ID\n\
    msp Latn BR\n\
    msq Latn NC\n\
    mss Latn ID\n\
    msu Latn PG\n\
    msv Latn CM\n\
    msw Latn GW\n\
    msx Latn PG\n\
    msy Latn PG\n\
    msz Latn PG\n\
    mta Latn PH\n\
    mtb Latn CI\n\
    mtc Latn PG\n\
    mtd Latn ID\n\
    mte Latn SB\n\
    mtf Latn PG\n\
    mtg Latn ID\n\
    mth Latn ID\n\
    mti Latn PG\n\
    mtj Latn ID\n\
    mtk Latn CM\n\
    mtl Latn NG\n\
    mtm Cyrl RU\n\
    mtn Latn NI\n\
    mto Latn MX\n\
    mtp Latn BO\n\
    mtq Latn VN\n\
    mts Latn PE\n\
    mtt Latn VU\n\
    mtu Latn MX\n\
    mtv Latn PG\n\
    mtw Latn PH\n\
    mtx Latn MX\n\
    mty Latn PG\n\
    mub Latn TD\n\
    muc Latn CM\n\
    mud Cyrl RU\n\
    mue Latn EC\n\
    mug Latn CM\n\
    muh Latn SS\n\
    mui Latn ID\n\
    muj Latn TD\n\
    muk Tibt NP\n\
    mum Latn PG\n\
    muo Latn CM\n\
    muq Latn CN\n\
    mur Latn SS\n\
    mut Deva IN\n\
    muu Latn KE\n\
    muv Taml IN\n\
    mux Latn PG\n\
    muy Latn CM\n\
    muz Ethi ET\n\
    mva Latn PG\n\
    mvd Latn ID\n\
    mve Arab PK\n\
    mvf Mong CN\n\
    mvg Latn MX\n\
    mvh Latn TD\n\
    mvk Latn PG\n\
    mvl Latn AU\n\
    mvn Latn PG\n\
    mvo Latn SB\n\
    mvp Latn ID\n\
    mvq Latn PG\n\
    mvr Latn ID\n\
    mvs Latn ID\n\
    mvt Latn VU\n\
    mvu Latn TD\n\
    mvv Latn MY\n\
    mvw Latn TZ\n\
    mvx Latn ID\n\
    mvz Ethi ET\n\
    mwa Latn PG\n\
    mwb Latn PG\n\
    mwc Latn PG\n\
    mwe Latn TZ\n\
    mwf Latn AU\n\
    mwg Latn PG\n\
    mwh Latn PG\n\
    mwi Latn VU\n\
    mwl Latn PT\n\
    mwm Latn TD\n\
    mwn Latn ZM\n\
    mwo Latn VU\n\
    mwp Latn AU\n\
    mwq Latn MM\n\
    mws Latn KE\n\
    mwt Mymr MM\n\
    mwu Latn SS\n\
    mwz Latn CD\n\
    mxa Latn MX\n\
    mxb Latn MX\n\
    mxd Latn ID\n\
    mxe Latn VU\n\
    mxf Latn CM\n\
    mxg Latn AO\n\
    mxh Latn CD\n\
    mxi Latn ES\n\
    mxj Latn IN\n\
    mxk Latn PG\n\
    mxl Latn BJ\n\
    mxm Latn PG\n\
    mxn Latn ID\n\
    mxo Latn ZM\n\
    mxp Latn MX\n\
    mxq Latn MX\n\
    mxr Latn MY\n\
    mxs Latn MX\n\
    mxt Latn MX\n\
    mxu Latn CM\n\
    mxv Latn MX\n\
    mxw Latn PG\n\
    mxx Latn CI\n\
    mxy Latn MX\n\
    mxz Latn ID\n\
    myb Latn TD\n\
    myc Latn CD\n\
    mye Latn GA\n\
    myf Latn ET\n\
    myg Latn CM\n\
    myh Latn US\n\
    myj Latn SS\n\
    myk Latn ML\n\
    myl Latn ID\n\
    mym Ethi ET\n\
    myp Latn BR\n\
    myr Latn PE\n\
    myu Latn BR\n\
    myw Latn PG\n\
    myy Latn CO\n\
    mza Latn MX\n\
    mzd Latn CM\n\
    mze Latn PG\n\
    mzh Latn AR\n\
    mzi Latn MX\n\
    mzj Latn LR\n\
    mzk Latn NG\n\
    mzl Latn MX\n\
    mzm Latn NG\n\
    mzo Latn BR\n\
    mzp Latn BO\n\
    mzq Latn ID\n\
    mzr Latn BR\n\
    mzt Latn MY\n\
    mzu Latn PG\n\
    mzv Latn CF\n\
    mzw Latn GH\n\
    mzx Latn GY\n\
    mzz Latn PG\n\
    naa Latn ID\n\
    nab Latn BR\n\
    nac Latn PG\n\
    nae Latn ID\n\
    naf Latn PG\n\
    nag Latn IN\n\
    naj Latn GN\n\
    nak Latn PG\n\
    nal Latn PG\n\
    nam Latn AU\n\
    nao Deva NP\n\
    nar Latn NG\n\
    nas Latn PG\n\
    nat Latn NG\n\
    naw Latn GH\n\
    nax Latn PG\n\
    nay Latn AU\n\
    naz Latn MX\n\
    nba Latn AO\n\
    nbb Latn NG\n\
    nbc Latn IN\n\
    nbd Latn CD\n\
    nbe Latn IN\n\
    nbh Latn NG\n\
    nbi Latn IN\n\
    nbj Latn AU\n\
    nbk Latn PG\n\
    nbm Latn CF\n\
    nbn Latn ID\n\
    nbo Latn NG\n\
    nbp Latn NG\n\
    nbq Latn ID\n\
    nbr Latn NG\n\
    nbt Latn IN\n\
    nbu Latn IN\n\
    nbv Latn CM\n\
    nbw Latn CD\n\
    nby Latn PG\n\
    nca Latn PG\n\
    ncb Latn IN\n\
    ncc Latn PG\n\
    ncd Deva NP\n\
    nce Latn PG\n\
    ncf Latn PG\n\
    ncg Latn CA\n\
    nci Latn MX\n\
    ncj Latn MX\n\
    nck Latn AU\n\
    ncl Latn MX\n\
    ncm Latn PG\n\
    ncn Latn PG\n\
    nco Latn PG\n\
    ncq Laoo LA\n\
    ncr Latn CM\n\
    nct Latn IN\n\
    ncu Latn GH\n\
    ncx Latn MX\n\
    ncz Latn US\n\
    nda Latn CG\n\
    ndb Latn CM\n\
    ndd Latn NG\n\
    ndf Cyrl RU\n\
    ndg Latn TZ\n\
    ndh Latn TZ\n\
    ndi Latn NG\n\
    ndj Latn TZ\n\
    ndk Latn CD\n\
    ndl Latn CD\n\
    ndm Latn TD\n\
    ndn Latn CG\n\
    ndp Latn UG\n\
    ndq Latn AO\n\
    ndr Latn NG\n\
    ndt Latn CD\n\
    ndu Latn CM\n\
    ndv Latn SN\n\
    ndw Latn CD\n\
    ndx Latn ID\n\
    ndy Latn CF\n\
    ndz Latn SS\n\
    nea Latn ID\n\
    neb Latn CI\n\
    nec Latn ID\n\
    ned Latn NG\n\
    nee Latn NC\n\
    neg Cyrl RU\n\
    neh Tibt BT\n\
    nei Xsux TR\n\
    nej Latn PG\n\
    nek Latn NC\n\
    nem Latn NC\n\
    nen Latn NC\n\
    neo Latn VN\n\
    neq Latn MX\n\
    ner Latn ID\n\
    net Latn PG\n\
    neu Latn 001\n\
    nex Latn PG\n\
    ney Latn CI\n\
    nez Latn US\n\
    nfa Latn ID\n\
    nfd Latn NG\n\
    nfl Latn SB\n\
    nfr Latn GH\n\
    nfu Latn CM\n\
    nga Latn CD\n\
    ngb Latn CD\n\
    ngc Latn CD\n\
    ngd Latn CF\n\
    nge Latn CM\n\
    ngg Latn CF\n\
    ngh Latn ZA\n\
    ngi Latn NG\n\
    ngj Latn CM\n\
    ngk Latn AU\n\
    ngm Latn FM\n\
    ngn Latn CM\n\
    ngp Latn TZ\n\
    ngq Latn TZ\n\
    ngr Latn SB\n\
    ngs Latn NG\n\
    ngt Laoo LA\n\
    ngu Latn MX\n\
    ngv Latn CM\n\
    ngw Latn NG\n\
    ngx Latn NG\n\
    ngy Latn CM\n\
    ngz Latn CG\n\
    nha Latn AU\n\
    nhb Latn CI\n\
    nhc Latn MX\n\
    nhd Latn PY\n\
    nhf Latn AU\n\
    nhg Latn MX\n\
    nhi Latn MX\n\
    nhk Latn MX\n\
    nhm Latn MX\n\
    nhn Latn MX\n\
    nho Latn PG\n\
    nhp Latn MX\n\
    nhq Latn MX\n\
    nhr Latn BW\n\
    nht Latn MX\n\
    nhu Latn CM\n\
    nhv Latn MX\n\
    nhx Latn MX\n\
    nhy Latn MX\n\
    nhz Latn MX\n\
    nia Latn ID\n\
    nib Latn PG\n\
    nid Latn AU\n\
    nie Latn TD\n\
    nif Latn PG\n\
    nig Latn AU\n\
    nih Latn TZ\n\
    nii Latn PG\n\
    nil Latn ID\n\
    nim Latn TZ\n\
    nin Latn NG\n\
    nio Cyrl RU\n\
    niq Latn KE\n\
    nir Latn ID\n\
    nis Latn PG\n\
    nit Telu IN\n\
    niv Cyrl RU\n\
    niw Latn PG\n\
    nix Latn CD\n\
    niy Latn CD\n\
    niz Latn PG\n\
    nja Latn NG\n\
    njb Latn IN\n\
    njd Latn TZ\n\
    njh Latn IN\n\
    nji Latn AU\n\
    njj Latn CM\n\
    njl Latn SS\n\
    njm Latn IN\n\
    njn Latn IN\n\
    njr Latn NG\n\
    njs Latn ID\n\
    njt Latn SR\n\
    nju Latn AU\n\
    njx Latn CG\n\
    njy Latn CM\n\
    njz Latn IN\n\
    nka Latn ZM\n\
    nkb Latn IN\n\
    nkc Latn CM\n\
    nkd Latn IN\n\
    nke Latn SB\n\
    nkf Latn IN\n\
    nkg Latn PG\n\
    nkh Latn IN\n\
    nki Latn IN\n\
    nkj Latn ID\n\
    nkk Latn VU\n\
    nkm Latn PG\n\
    nkn Latn AO\n\
    nko Latn GH\n\
    nkq Latn GH\n\
    nkr Latn FM\n\
    nks Latn ID\n\
    nkt Latn TZ\n\
    nku Latn CI\n\
    nkv Latn MW\n\
    nkw Latn CD\n\
    nkx Latn NG\n\
    nkz Latn NG\n\
    nla Latn CM\n\
    nlc Latn ID\n\
    nle Latn KE\n\
    nlg Latn SB\n\
    nli Arab AF\n\
    nlj Latn CD\n\
    nlk Latn ID\n\
    nlm Arab PK\n\
    nlo Latn CD\n\
    nlq Latn MM\n\
    nlu Latn GH\n\
    nlv Latn MX\n\
    nlw Latn AU\n\
    nlx Deva IN\n\
    nly Latn AU\n\
    nlz Latn SB\n\
    nma Latn IN\n\
    nmb Latn VU\n\
    nmc Latn TD\n\
    nmd Latn GA\n\
    nme Latn IN\n\
    nmf Latn IN\n\
    nmh Latn IN\n\
    nmi Latn NG\n\
    nmj Latn CF\n\
    nmk Latn VU\n\
    nml Latn CM\n\
    nmm Deva NP\n\
    nmn Latn BW\n\
    nmo Latn IN\n\
    nmp Latn AU\n\
    nmq Latn ZW\n\
    nmr Latn CM\n\
    nms Latn VU\n\
    nmt Latn FM\n\
    nmu Latn US\n\
    nmv Latn AU\n\
    nmw Latn PG\n\
    nmx Latn PG\n\
    nmz Latn TG\n\
    nna Latn AU\n\
    nnb Latn CD\n\
    nnc Latn TD\n\
    nnd Latn VU\n\
    nne Latn AO\n\
    nnf Latn PG\n\
    nng Latn IN\n\
    nni Latn ID\n\
    nnj Latn ET\n\
    nnk Latn PG\n\
    nnl Latn IN\n\
    nnm Latn PG\n\
    nnn Latn TD\n\
    nnq Latn TZ\n\
    nnr Latn AU\n\
    nnt Latn US\n\
    nnu Latn GH\n\
    nnv Latn AU\n\
    nnw Latn BF\n\
    nny Latn AU\n\
    nnz Latn CM\n\
    noa Latn CO\n\
    noc Latn PG\n\
    nof Latn PG\n\
    nog Cyrl RU\n\
    noh Latn PG\n\
    noi Deva IN\n\
    noj Latn CO\n\
    nok Latn US\n\
    nop Latn PG\n\
    noq Latn CD\n\
    nos Yiii CN\n\
    not Latn PE\n\
    nou Latn PG\n\
    nov Latn 001\n\
    now Latn TZ\n\
    noy Latn TD\n\
    npb Tibt BT\n\
    npg Latn MM\n\
    nph Latn IN\n\
    npl Latn MX\n\
    npn Latn PG\n\
    npo Latn IN\n\
    nps Latn ID\n\
    npu Latn IN\n\
    npx Latn SB\n\
    npy Latn ID\n\
    nqg Latn BJ\n\
    nqk Latn BJ\n\
    nql Latn AO\n\
    nqm Latn ID\n\
    nqn Latn PG\n\
    nqq Latn MM\n\
    nqt Latn NG\n\
    nqy Latn MM\n\
    nra Latn GA\n\
    nrb Latn ER\n\
    nre Latn IN\n\
    nrf Latn JE\n\
    nrg Latn VU\n\
    nri Latn IN\n\
    nrk Latn AU\n\
    nrl Latn AU\n\
    nrm Latn MY\n\
    nrn Runr GB\n\
    nrp Latn IT\n\
    nru Latn CN\n\
    nrx Latn AU\n\
    nrz Latn PG\n\
    nsa Latn IN\n\
    nsb Latn ZA\n\
    nsc Latn NG\n\
    nsd Yiii CN\n\
    nsf Yiii CN\n\
    nsg Latn TZ\n\
    nsh Latn CM\n\
    nsm Latn IN\n\
    nsn Latn PG\n\
    nsq Latn US\n\
    nss Latn PG\n\
    nsu Latn MX\n\
    nsv Yiii CN\n\
    nsw Latn VU\n\
    nsx Latn AO\n\
    nsy Latn ID\n\
    nsz Latn US\n\
    ntd Latn MY\n\
    nte Latn MZ\n\
    ntg Latn AU\n\
    nti Latn BF\n\
    ntj Latn AU\n\
    ntk Latn TZ\n\
    ntm Latn BJ\n\
    nto Latn CD\n\
    ntp Latn MX\n\
    ntr Latn GH\n\
    ntu Latn SB\n\
    ntx Latn MM\n\
    nty Yiii VN\n\
    ntz Arab IR\n\
    nua Latn NC\n\
    nuc Latn BR\n\
    nud Latn PG\n\
    nue Latn CD\n\
    nuf Latn CN\n\
    nug Latn AU\n\
    nuh Latn NG\n\
    nui Latn GQ\n\
    nuj Latn UG\n\
    nuk Latn CA\n\
    num Latn TO\n\
    nun Latn MM\n\
    nuo Latn VN\n\
    nup Latn NG\n\
    nuq Latn PG\n\
    nur Latn PG\n\
    nut Latn VN\n\
    nuu Latn CD\n\
    nuv Latn BF\n\
    nuw Latn FM\n\
    nux Latn PG\n\
    nuy Latn AU\n\
    nuz Latn MX\n\
    nvh Latn VU\n\
    nvm Latn PG\n\
    nvo Latn CM\n\
    nwb Latn CI\n\
    nwc Newa NP\n\
    nwe Latn CM\n\
    nwg Latn AU\n\
    nwi Latn VU\n\
    nwm Latn SS\n\
    nwo Latn AU\n\
    nwr Latn PG\n\
    nww Latn TZ\n\
    nwx Deva NP\n\
    nxa Latn TL\n\
    nxd Latn CD\n\
    nxe Latn ID\n\
    nxg Latn ID\n\
    nxi Latn TZ\n\
    nxl Latn ID\n\
    nxn Latn AU\n\
    nxo Latn GA\n\
    nxr Latn PG\n\
    nxx Latn ID\n\
    nyb Latn GH\n\
    nyc Latn CD\n\
    nyd Latn KE\n\
    nye Latn AO\n\
    nyf Latn KE\n\
    nyg Latn CD\n\
    nyh Latn AU\n\
    nyi Latn SD\n\
    nyj Latn CD\n\
    nyk Latn AO\n\
    nyl Thai TH\n\
    nyo Latn UG\n\
    nyp Latn UG\n\
    nyq Arab IR\n\
    nyr Latn MW\n\
    nys Latn AU\n\
    nyt Latn AU\n\
    nyu Latn MZ\n\
    nyv Latn AU\n\
    nyw Thai TH\n\
    nyx Latn AU\n\
    nyy Latn TZ\n\
    nza Latn CM\n\
    nzb Latn GA\n\
    nzd Latn CD\n\
    nzk Latn CF\n\
    nzm Latn IN\n\
    nzr Latn NG\n\
    nzu Latn CG\n\
    nzy Latn TD\n\
    nzz Latn ML\n\
    oaa Cyrl RU\n\
    oac Cyrl RU\n\
    oar Syrc SY\n\
    oav Geor GE\n\
    obi Latn US\n\
    obk Latn PH\n\
    obl Latn CM\n\
    obm Phnx JO\n\
    obo Latn PH\n\
    obr Mymr MM\n\
    obt Latn FR\n\
    obu Latn NG\n\
    oca Latn PE\n\
    oco Latn GB\n\
    ocu Latn MX\n\
    oda Latn NG\n\
    odk Arab PK\n\
    odt Latn NL\n\
    odu Latn NG\n\
    ofs Latn NL\n\
    ofu Latn NG\n\
    ogb Latn NG\n\
    ogc Latn NG\n\
    ogg Latn NG\n\
    ogo Latn NG\n\
    ogu Latn NG\n\
    oht Xsux TR\n\
    ohu Latn HU\n\
    oia Latn ID\n\
    oie Latn SS\n\
    oin Latn PG\n\
    ojb Latn CA\n\
    ojc Latn CA\n\
    ojv Latn SB\n\
    okb Latn NG\n\
    okc Latn CD\n\
    okd Latn NG\n\
    oke Latn NG\n\
    okg Latn AU\n\
    oki Latn KE\n\
    okk Latn PG\n\
    okm Hang KR\n\
    oko Hani KR\n\
    okr Latn NG\n\
    oks Latn NG\n\
    oku Latn CM\n\
    okv Latn PG\n\
    okx Latn NG\n\
    okz Khmr KH\n\
    ola Deva NP\n\
    old Latn TZ\n\
    ole Tibt BT\n\
    olk Latn AU\n\
    olm Latn NG\n\
    olo Latn RU\n\
    olr Latn VU\n\
    olt Latn LT\n\
    olu Latn AO\n\
    oma Latn US\n\
    omb Latn VU\n\
    omc Latn PE\n\
    omg Latn PE\n\
    omi Latn CD\n\
    omk Cyrl RU\n\
    oml Latn CD\n\
    omo Latn PG\n\
    omp Mtei IN\n\
    omr Modi IN\n\
    omt Latn KE\n\
    omu Latn PE\n\
    omw Latn PG\n\
    omx Mymr MM\n\
    ona Latn AR\n\
    one Latn CA\n\
    ong Latn PG\n\
    oni Latn ID\n\
    onj Latn PG\n\
    onk Latn PG\n\
    onn Latn PG\n\
    ono Latn CA\n\
    onp Latn IN\n\
    onr Latn PG\n\
    ons Latn PG\n\
    ont Latn PG\n\
    onu Latn VU\n\
    onx Latn ID\n\
    ood Latn US\n\
    oon Deva IN\n\
    oor Latn ZA\n\
    opa Latn NG\n\
    opk Latn ID\n\
    opm Latn PG\n\
    opo Latn PG\n\
    opt Latn MX\n\
    opy Latn BR\n\
    ora Latn SB\n\
    orc Latn KE\n\
    ore Latn PE\n\
    org Latn NG\n\
    orn Latn MY\n\
    oro Latn PG\n\
    orr Latn NG\n\
    ors Latn MY\n\
    ort Telu IN\n\
    oru Arab PK\n\
    orv Cyrl RU\n\
    orw Latn BR\n\
    orx Latn NG\n\
    orz Latn ID\n\
    osc Ital IT\n\
    osi Java ID\n\
    oso Latn NG\n\
    osp Latn ES\n\
    ost Latn CM\n\
    osu Latn PG\n\
    osx Latn DE\n\
    ota Arab TR\n\
    otb Tibt CN\n\
    otd Latn ID\n\
    ote Latn MX\n\
    oti Latn BR\n\
    otl Latn MX\n\
    otm Latn MX\n\
    otn Latn MX\n\
    otq Latn MX\n\
    otr Latn SD\n\
    ots Latn MX\n\
    ott Latn MX\n\
    otu Latn BR\n\
    otw Latn CA\n\
    otx Latn MX\n\
    oty Gran IN\n\
    otz Latn MX\n\
    oub Latn LR\n\
    oue Latn PG\n\
    oum Latn PG\n\
    ovd Latn SE\n\
    owi Latn PG\n\
    owl Latn GB\n\
    oyd Latn ET\n\
    oym Latn BR\n\
    oyy Latn PG\n\
    ozm Latn CM\n\
    pab Latn BR\n\
    pac Latn VN\n\
    pad Latn BR\n\
    pae Latn CD\n\
    paf Latn BR\n\
    pah Latn BR\n\
    pai Latn NG\n\
    pak Latn BR\n\
    pao Latn US\n\
    paq Cyrl TJ\n\
    par Latn US\n\
    pas Latn ID\n\
    pav Latn BR\n\
    paw Latn US\n\
    pax Latn BR\n\
    pay Latn HN\n\
    paz Latn BR\n\
    pbb Latn CO\n\
    pbc Latn GY\n\
    pbe Latn MX\n\
    pbf Latn MX\n\
    pbg Latn VE\n\
    pbh Latn VE\n\
    pbi Latn CM\n\
    pbl Latn NG\n\
    pbm Latn MX\n\
    pbn Latn NG\n\
    pbo Latn GW\n\
    pbp Latn GN\n\
    pbr Latn TZ\n\
    pbs Latn MX\n\
    pbt Arab AF\n\
    pbv Latn IN\n\
    pby Latn PG\n\
    pca Latn MX\n\
    pcb Khmr KH\n\
    pcc Latn CN\n\
    pce Mymr MM\n\
    pcf Mlym IN\n\
    pcg Mlym IN\n\
    pch Deva IN\n\
    pci Deva IN\n\
    pcj Telu IN\n\
    pck Latn IN\n\
    pcn Latn NG\n\
    pcp Latn BO\n\
    pcw Latn NG\n\
    pda Latn PG\n\
    pdn Latn ID\n\
    pdo Latn ID\n\
    pdu Latn MM\n\
    pea Latn ID\n\
    peb Latn US\n\
    ped Latn PG\n\
    pee Latn ID\n\
    peg Orya IN\n\
    pei Latn MX\n\
    pek Latn PG\n\
    pel Latn ID\n\
    pem Latn CD\n\
    pep Latn PG\n\
    peq Latn US\n\
    pev Latn VE\n\
    pex Latn PG\n\
    pey Latn ID\n\
    pez Latn MY\n\
    pfa Latn FM\n\
    pfe Latn CM\n\
    pga Latn SS\n\
    pgd Khar PK\n\
    pgg Deva IN\n\
    pgi Latn PG\n\
    pgk Latn VU\n\
    pgl Ogam IE\n\
    pgn Ital IT\n\
    pgs Latn NG\n\
    pgu Latn ID\n\
    phd Deva IN\n\
    phg Latn VN\n\
    phh Latn VN\n\
    phk Mymr IN\n\
    phl Arab PK\n\
    phm Latn MZ\n\
    pho Laoo LA\n\
    phr Arab PK\n\
    pht Thai TH\n\
    phu Thai TH\n\
    phv Arab AF\n\
    phw Deva NP\n\
    pi Sinh IN\n\
    pia Latn MX\n\
    pib Latn PE\n\
    pic Latn GA\n\
    pid Latn VE\n\
    pif Latn FM\n\
    pig Latn PE\n\
    pih Latn NF\n\
    pij Latn CO\n\
    pil Latn BJ\n\
    pim Latn US\n\
    pin Latn PG\n\
    pio Latn CO\n\
    pip Latn NG\n\
    pir Latn BR\n\
    pit Latn AU\n\
    piu Latn AU\n\
    piv Latn SB\n\
    piw Latn TZ\n\
    pix Latn PG\n\
    piy Latn NG\n\
    piz Latn NC\n\
    pjt Latn AU\n\
    pkb Latn KE\n\
    pkg Latn PG\n\
    pkh Latn BD\n\
    pkn Latn AU\n\
    pkp Latn CK\n\
    pkr Mlym IN\n\
    pku Latn ID\n\
    pla Latn PG\n\
    plb Latn VU\n\
    plc Latn PH\n\
    pld Latn GB\n\
    ple Latn ID\n\
    plg Latn AR\n\
    plh Latn ID\n\
    plk Arab PK\n\
    pll Mymr MM\n\
    pln Latn CO\n\
    plo Latn MX\n\
    plr Latn CI\n\
    pls Latn MX\n\
    plu Latn BR\n\
    plv Latn PH\n\
    plw Latn PH\n\
    plz Latn MY\n\
    pma Latn VU\n\
    pmb Latn CD\n\
    pmd Latn AU\n\
    pme Latn NC\n\
    pmf Latn ID\n\
    pmh Brah IN\n\
    pmi Latn CN\n\
    pmj Latn CN\n\
    pml Latn TN\n\
    pmm Latn CM\n\
    pmn Latn CM\n\
    pmo Latn ID\n\
    pmq Latn MX\n\
    pmr Latn PG\n\
    pmt Latn PF\n\
    pmw Latn US\n\
    pmx Latn IN\n\
    pmy Latn ID\n\
    pmz Latn MX\n\
    pna Latn MY\n\
    pnc Latn ID\n\
    pnd Latn AO\n\
    pne Latn MY\n\
    png Latn NG\n\
    pnh Latn CK\n\
    pni Latn ID\n\
    pnj Latn AU\n\
    pnk Latn BO\n\
    pnl Latn BF\n\
    pnm Latn MY\n\
    pnn Latn PG\n\
    pno Latn PE\n\
    pnp Latn ID\n\
    pnq Latn BF\n\
    pnr Latn PG\n\
    pns Latn ID\n\
    pnv Latn AU\n\
    pnw Latn AU\n\
    pny Latn CM\n\
    pnz Latn CF\n\
    poc Latn GT\n\
    poe Latn MX\n\
    pof Latn CD\n\
    pog Latn BR\n\
    poh Latn GT\n\
    poi Latn MX\n\
    pok Latn BR\n\
    pom Latn US\n\
    poo Latn US\n\
    pop Latn NC\n\
    poq Latn MX\n\
    pos Latn MX\n\
    pot Latn US\n\
    pov Latn GW\n\
    pow Latn MX\n\
    poy Latn TZ\n\
    ppe Latn PG\n\
    ppi Latn MX\n\
    ppk Latn ID\n\
    ppm Latn ID\n\
    ppn Latn PG\n\
    ppo Latn PG\n\
    ppp Latn CD\n\
    ppq Latn PG\n\
    pps Latn MX\n\
    ppt Latn PG\n\
    pqa Latn NG\n\
    prc Arab AF\n\
    pre Latn ST\n\
    prf Latn PH\n\
    prh Latn PH\n\
    pri Latn NC\n\
    prk Latn MM\n\
    prm Latn PG\n\
    pro Latn FR\n\
    prq Latn PE\n\
    prr Latn BR\n\
    prt Thai TH\n\
    pru Latn ID\n\
    prw Latn PG\n\
    prx Arab IN\n\
    psa Latn ID\n\
    pse Latn ID\n\
    psh Arab AF\n\
    psi Arab AF\n\
    psm Latn BO\n\
    psn Latn ID\n\
    psq Latn PG\n\
    pss Latn PG\n\
    pst Arab PK\n\
    psu Brah IN\n\
    psw Latn VU\n\
    pta Latn PY\n\
    pth Latn BR\n\
    pti Latn AU\n\
    ptn Latn ID\n\
    pto Latn BR\n\
    ptp Latn PG\n\
    ptr Latn VU\n\
    ptt Latn ID\n\
    ptu Latn ID\n\
    ptv Latn VU\n\
    pua Latn MX\n\
    pub Latn IN\n\
    puc Latn ID\n\
    pud Latn ID\n\
    pue Latn AR\n\
    puf Latn ID\n\
    pug Latn BF\n\
    pui Latn CO\n\
    puj Latn ID\n\
    pum Deva NP\n\
    puo Latn VN\n\
    pup Latn PG\n\
    puq Latn BO\n\
    pur Latn BR\n\
    put Latn ID\n\
    puw Latn FM\n\
    pux Latn PG\n\
    puy Latn US\n\
    pwa Latn PG\n\
    pwb Latn NG\n\
    pwg Latn PG\n\
    pwm Latn PH\n\
    pwn Latn TW\n\
    pwo Mymr MM\n\
    pwr Deva IN\n\
    pww Thai TH\n\
    pxm Latn MX\n\
    pye Latn CI\n\
    pym Latn NG\n\
    pyn Latn BR\n\
    pyu Latn TW\n\
    pyx Mymr MM\n\
    pyy Latn MM\n\
    pze Latn NG\n\
    pzh Latn TW\n\
    pzn Latn MM\n\
    qua Latn US\n\
    qub Latn PE\n\
    qud Latn EC\n\
    quf Latn PE\n\
    qui Latn US\n\
    quk Latn PE\n\
    qul Latn BO\n\
    qum Latn GT\n\
    qun Latn US\n\
    qup Latn PE\n\
    quq Latn ES\n\
    qur Latn PE\n\
    qus Latn AR\n\
    quv Latn GT\n\
    quw Latn EC\n\
    qux Latn PE\n\
    quy Latn PE\n\
    qva Latn PE\n\
    qvc Latn PE\n\
    qve Latn PE\n\
    qvh Latn PE\n\
    qvi Latn EC\n\
    qvj Latn EC\n\
    qvl Latn PE\n\
    qvm Latn PE\n\
    qvn Latn PE\n\
    qvo Latn PE\n\
    qvp Latn PE\n\
    qvs Latn PE\n\
    qvw Latn PE\n\
    qvz Latn EC\n\
    qwa Latn PE\n\
    qwc Latn PE\n\
    qwh Latn PE\n\
    qwm Latn HU\n\
    qws Latn PE\n\
    qwt Latn US\n\
    qxa Latn PE\n\
    qxc Latn PE\n\
    qxh Latn PE\n\
    qxl Latn EC\n\
    qxn Latn PE\n\
    qxo Latn PE\n\
    qxp Latn PE\n\
    qxq Arab IR\n\
    qxr Latn EC\n\
    qxt Latn PE\n\
    qxu Latn PE\n\
    qxw Latn PE\n\
    qya Latn 001\n\
    qyp Latn US\n\
    raa Deva NP\n\
    rab Deva NP\n\
    rac Latn ID\n\
    rad Latn VN\n\
    raf Deva NP\n\
    rag Latn KE\n\
    rah Beng IN\n\
    rai Latn PG\n\
    rak Latn PG\n\
    ram Latn BR\n\
    ran Latn ID\n\
    rao Latn PG\n\
    rap Latn CL\n\
    rar Latn CK\n\
    rav Deva NP\n\
    raw Latn MM\n\
    rax Latn NG\n\
    ray Latn PF\n\
    raz Latn ID\n\
    rbb Mymr MM\n\
    rbk Latn PH\n\
    rbl Latn PH\n\
    rbp Latn AU\n\
    rdb Arab IR\n\
    rea Latn PG\n\
    reb Latn ID\n\
    ree Latn MY\n\
    reg Latn TZ\n\
    rei Orya IN\n\
    rel Latn KE\n\
    rem Latn PE\n\
    ren Latn VN\n\
    res Latn NG\n\
    ret Latn ID\n\
    rey Latn BO\n\
    rga Latn VU\n\
    rgr Latn PE\n\
    rgs Latn VN\n\
    rgu Latn ID\n\
    rhp Latn PG\n\
    ril Latn MM\n\
    rim Latn TZ\n\
    rin Latn NG\n\
    rir Latn ID\n\
    rit Latn AU\n\
    riu Latn ID\n\
    rjg Latn ID\n\
    rji Deva NP\n\
    rka Khmr KH\n\
    rkb Latn BR\n\
    rkh Latn CK\n\
    rki Mymr MM\n\
    rkm Latn BF\n\
    rkw Latn AU\n\
    rma Latn NI\n\
    rmb Latn AU\n\
    rmc Latn SK\n\
    rmd Latn DK\n\
    rme Latn GB\n\
    rmg Latn NO\n\
    rmh Latn ID\n\
    rmi Armn AM\n\
    rmk Latn PG\n\
    rml Latn PL\n\
    rmm Latn ID\n\
    rmn Latn RS\n\
    rmp Latn PG\n\
    rmq Latn ES\n\
    rmw Latn GB\n\
    rmx Latn VN\n\
    rmz Mymr IN\n\
    rnd Latn CD\n\
    rnl Latn IN\n\
    rnn Latn ID\n\
    rnr Latn AU\n\
    rnw Latn TZ\n\
    roc Latn VN\n\
    rod Latn NG\n\
    roe Latn PG\n\
    rog Latn VN\n\
    rol Latn PH\n\
    rom Latn RO\n\
    roo Latn PG\n\
    rop Latn AU\n\
    ror Latn ID\n\
    rou Latn TD\n\
    row Latn ID\n\
    rpn Latn VU\n\
    rpt Latn PG\n\
    rri Latn SB\n\
    rrm Latn NZ\n\
    rro Latn PG\n\
    rrt Latn AU\n\
    rsk Cyrl RS\n\
    rsw Latn NG\n\
    rtc Latn MM\n\
    rth Latn ID\n\
    rtw Deva IN\n\
    rub Latn UG\n\
    ruc Latn UG\n\
    ruf Latn TZ\n\
    rui Latn TZ\n\
    ruk Latn NG\n\
    ruo Latn HR\n\
    rup Latn RO\n\
    ruq Latn GR\n\
    rut Cyrl RU\n\
    ruu Latn MY\n\
    ruy Latn NG\n\
    ruz Latn NG\n\
    rwa Latn PG\n\
    rwl Latn TZ\n\
    rwm Latn UG\n\
    rwo Latn PG\n\
    rwr Deva IN\n\
    rxd Latn AU\n\
    rxw Latn AU\n\
    saa Latn TD\n\
    sab Latn PA\n\
    sac Latn US\n\
    sad Latn TZ\n\
    sae Latn BR\n\
    saj Latn ID\n\
    sak Latn GA\n\
    sam Samr PS\n\
    sao Latn ID\n\
    sar Latn BO\n\
    sau Latn ID\n\
    saw Latn ID\n\
    sax Latn VU\n\
    say Latn NG\n\
    sba Latn TD\n\
    sbb Latn SB\n\
    sbc Latn PG\n\
    sbd Latn BF\n\
    sbe Latn PG\n\
    sbg Latn ID\n\
    sbh Latn PG\n\
    sbi Latn PG\n\
    sbj Latn TD\n\
    sbk Latn TZ\n\
    sbl Latn PH\n\
    sbm Latn TZ\n\
    sbn Arab PK\n\
    sbo Latn MY\n\
    sbq Latn PG\n\
    sbr Latn ID\n\
    sbs Latn NA\n\
    sbt Latn ID\n\
    sbu Tibt IN\n\
    sbv Latn IT\n\
    sbw Latn GA\n\
    sbx Latn ID\n\
    sby Latn ZM\n\
    sbz Latn CF\n\
    scb Latn VN\n\
    sce Latn CN\n\
    scf Latn PA\n\
    scg Latn ID\n\
    sch Latn IN\n\
    sci Latn LK\n\
    scl Arab PK\n\
    scp Deva NP\n\
    scs Latn CA\n\
    sct Laoo LA\n\
    scu Takr IN\n\
    scv Latn NG\n\
    scw Latn NG\n\
    scx Grek IT\n\
    sda Latn ID\n\
    sdb Arab IQ\n\
    sde Latn NG\n\
    sdf Arab IQ\n\
    sdg Arab AF\n\
    sdj Latn CG\n\
    sdk Latn PG\n\
    sdn Latn IT\n\
    sdo Latn MY\n\
    sdq Latn ID\n\
    sdr Beng BD\n\
    sds Arab TN\n\
    sdu Latn ID\n\
    sdx Latn MY\n\
    sea Latn MY\n\
    seb Latn CI\n\
    sec Latn CA\n\
    sed Latn VN\n\
    see Latn US\n\
    seg Latn TZ\n\
    sej Latn PG\n\
    sek Latn CA\n\
    sel Cyrl RU\n\
    sen Latn BF\n\
    seo Latn PG\n\
    sep Latn BF\n\
    seq Latn BF\n\
    ser Latn US\n\
    set Latn ID\n\
    seu Latn ID\n\
    sev Latn CI\n\
    sew Latn PG\n\
    sey Latn EC\n\
    sez Latn MM\n\
    sfe Latn PH\n\
    sfm Plrd CN\n\
    sfw Latn GH\n\
    sgb Latn PH\n\
    sgc Latn KE\n\
    sgd Latn PH\n\
    sge Latn ID\n\
    sgh Cyrl TJ\n\
    sgi Latn CM\n\
    sgj Deva IN\n\
    sgm Latn KE\n\
    sgp Latn IN\n\
    sgr Arab IR\n\
    sgt Tibt BT\n\
    sgu Latn ID\n\
    sgw Ethi ET\n\
    sgy Arab AF\n\
    sgz Latn PG\n\
    sha Latn NG\n\
    shb Latn BR\n\
    shc Latn CD\n\
    shd Arab PK\n\
    she Latn ET\n\
    shg Latn BW\n\
    shh Latn US\n\
    shj Latn SD\n\
    shk Latn SS\n\
    shm Arab IR\n\
    sho Latn NG\n\
    shp Latn PE\n\
    shq Latn ZM\n\
    shr Latn CD\n\
    shs Latn CA\n\
    sht Latn US\n\
    shu Arab TD\n\
    shv Arab OM\n\
    shw Latn SD\n\
    shy Latn DZ\n\
    shz Latn ML\n\
    sia Cyrl RU\n\
    sib Latn MY\n\
    sie Latn ZM\n\
    sif Latn BF\n\
    sig Latn GH\n\
    sih Latn NC\n\
    sii Latn IN\n\
    sij Latn PG\n\
    sik Latn BR\n\
    sil Latn GH\n\
    sim Latn PG\n\
    sip Tibt IN\n\
    siq Latn PG\n\
    sir Latn NG\n\
    sis Latn US\n\
    siu Latn PG\n\
    siv Latn PG\n\
    siw Latn PG\n\
    six Latn PG\n\
    siy Arab IR\n\
    siz Arab EG\n\
    sja Latn CO\n\
    sjb Latn ID\n\
    sjd Cyrl RU\n\
    sje Latn SE\n\
    sjg Latn TD\n\
    sjl Latn IN\n\
    sjm Latn PH\n\
    sjp Deva IN\n\
    sjr Latn PG\n\
    sjt Cyrl RU\n\
    sju Latn SE\n\
    sjw Latn US\n\
    ska Latn US\n\
    skb Thai TH\n\
    skc Latn PG\n\
    skd Latn US\n\
    ske Latn VU\n\
    skf Latn BR\n\
    skg Latn MG\n\
    skh Latn ID\n\
    ski Latn ID\n\
    skj Deva NP\n\
    skm Latn PG\n\
    skn Latn PH\n\
    sko Latn ID\n\
    skp Latn MY\n\
    skq Latn BF\n\
    sks Latn PG\n\
    skt Latn CD\n\
    sku Latn VU\n\
    skv Latn ID\n\
    skw Latn GY\n\
    skx Latn ID\n\
    sky Latn SB\n\
    skz Latn ID\n\
    slc Latn CO\n\
    sld Latn BF\n\
    slg Latn ID\n\
    slh Latn US\n\
    slj Latn BR\n\
    sll Latn PG\n\
    slm Latn PH\n\
    sln Latn US\n\
    slp Latn ID\n\
    slr Latn CN\n\
    slu Latn ID\n\
    slw Latn PG\n\
    slx Latn CD\n\
    slz Latn ID\n\
    smb Latn PG\n\
    smc Latn PG\n\
    smf Latn PG\n\
    smg Latn PG\n\
    smh Yiii CN\n\
    smk Latn PH\n\
    sml Latn PH\n\
    smq Latn PG\n\
    smr Latn ID\n\
    smt Latn IN\n\
    smu Khmr KH\n\
    smw Latn ID\n\
    smx Latn CD\n\
    smy Arab IR\n\
    smz Latn PG\n\
    snc Latn PG\n\
    sne Latn MY\n\
    sng Latn CD\n\
    sni Latn PE\n\
    snj Latn CF\n\
    snl Latn PH\n\
    snm Latn UG\n\
    snn Latn CO\n\
    sno Latn US\n\
    snp Latn PG\n\
    snq Latn GA\n\
    snr Latn PG\n\
    sns Latn VU\n\
    snu Latn ID\n\
    snv Latn MY\n\
    snw Latn GH\n\
    snx Latn PG\n\
    sny Latn PG\n\
    snz Latn PG\n\
    soa Tavt TH\n\
    sob Latn ID\n\
    soc Latn CD\n\
    sod Latn CD\n\
    soe Latn CD\n\
    soi Deva NP\n\
    sok Latn TD\n\
    sol Latn PG\n\
    soo Latn CD\n\
    sop Latn CD\n\
    soq Latn PG\n\
    sor Latn TD\n\
    sos Latn BF\n\
    sov Latn PW\n\
    sow Latn PG\n\
    sox Latn CM\n\
    soy Latn BJ\n\
    soz Latn TZ\n\
    spb Latn ID\n\
    spc Latn VE\n\
    spd Latn PG\n\
    spe Latn PG\n\
    spg Latn MY\n\
    spi Latn ID\n\
    spk Latn PG\n\
    spl Latn PG\n\
    spm Latn PG\n\
    spn Latn PY\n\
    spo Latn US\n\
    spp Latn ML\n\
    spq Latn PE\n\
    spr Latn ID\n\
    sps Latn PG\n\
    spt Tibt IN\n\
    spv Orya IN\n\
    sqa Latn NG\n\
    sqh Latn NG\n\
    sqm Latn CF\n\
    sqo Arab IR\n\
    sqq Laoo LA\n\
    sqt Arab YE\n\
    squ Latn CA\n\
    sra Latn PG\n\
    sre Latn ID\n\
    srf Latn PG\n\
    srg Latn PH\n\
    srh Arab CN\n\
    sri Latn CO\n\
    srk Latn MY\n\
    srl Latn ID\n\
    srm Latn SR\n\
    sro Latn IT\n\
    srq Latn BO\n\
    srs Latn CA\n\
    srt Latn ID\n\
    sru Latn BR\n\
    srv Latn PH\n\
    srw Latn ID\n\
    sry Latn PG\n\
    srz Arab IR\n\
    ssb Latn PH\n\
    ssc Latn TZ\n\
    ssd Latn PG\n\
    sse Latn PH\n\
    ssf Latn TW\n\
    ssg Latn PG\n\
    ssh Arab AE\n\
    ssj Latn PG\n\
    ssl Latn GH\n\
    ssm Latn MY\n\
    ssn Latn KE\n\
    sso Latn PG\n\
    ssq Latn ID\n\
    sss Laoo LA\n\
    sst Latn PG\n\
    ssu Latn PG\n\
    ssv Latn VU\n\
    ssx Latn PG\n\
    ssz Latn PG\n\
    sta Latn ZM\n\
    stb Latn PH\n\
    ste Latn ID\n\
    stf Latn PG\n\
    stg Latn VN\n\
    sth Latn IE\n\
    sti Latn VN\n\
    stj Latn BF\n\
    stk Latn PG\n\
    stl Latn NL\n\
    stm Latn PG\n\
    stn Latn SB\n\
    sto Latn CA\n\
    stp Latn MX\n\
    str Latn CA\n\
    sts Arab AF\n\
    stt Latn VN\n\
    stv Ethi ET\n\
    stw Latn FM\n\
    sty Cyrl RU\n\
    sua Latn PG\n\
    sub Latn CD\n\
    suc Latn PH\n\
    sue Latn PG\n\
    sug Latn PG\n\
    sui Latn PG\n\
    suj Latn TZ\n\
    suo Latn PG\n\
    suq Latn ET\n\
    sur Latn NG\n\
    sut Latn NI\n\
    suv Latn IN\n\
    suw Latn TZ\n\
    suy Latn BR\n\
    sva Geor GE\n\
    svb Latn PG\n\
    svc Latn VC\n\
    sve Latn ID\n\
    svm Latn IT\n\
    svs Latn SB\n\
    swf Latn CD\n\
    swi Hani CN\n\
    swj Latn GA\n\
    swk Latn MW\n\
    swm Latn PG\n\
    swo Latn BR\n\
    swp Latn PG\n\
    swq Latn CM\n\
    swr Latn ID\n\
    sws Latn ID\n\
    swt Latn ID\n\
    swu Latn ID\n\
    sww Latn VU\n\
    swx Latn BR\n\
    swy Latn TD\n\
    sxb Latn KE\n\
    sxe Latn GA\n\
    sxr Latn TW\n\
    sxs Latn NG\n\
    sxu Runr DE\n\
    sxw Latn BJ\n\
    sya Latn ID\n\
    syb Latn PH\n\
    syc Syrc TR\n\
    syi Latn GA\n\
    syk Latn NG\n\
    sym Latn BF\n\
    syn Syrc IR\n\
    syo Latn KH\n\
    sys Latn TD\n\
    syw Deva NP\n\
    syx Latn GA\n\
    sza Latn MY\n\
    szb Latn ID\n\
    szc Latn MY\n\
    szg Latn CD\n\
    szn Latn ID\n\
    szp Latn ID\n\
    szv Latn CM\n\
    szw Latn ID\n\
    szy Latn TW\n\
    taa Latn US\n\
    tab Cyrl RU\n\
    tac Latn MX\n\
    tad Latn ID\n\
    tae Latn BR\n\
    taf Latn BR\n\
    tag Latn SD\n\
    tak Latn NG\n\
    tal Latn NG\n\
    tan Latn NG\n\
    tao Latn TW\n\
    tap Latn CD\n\
    taq Latn ML\n\
    tar Latn MX\n\
    tas Latn VN\n\
    tau Latn US\n\
    tav Latn CO\n\
    taw Latn PG\n\
    tax Latn TD\n\
    tay Latn TW\n\
    taz Latn SD\n\
    tba Latn BR\n\
    tbc Latn PG\n\
    tbd Latn PG\n\
    tbe Latn SB\n\
    tbf Latn PG\n\
    tbg Latn PG\n\
    tbh Latn AU\n\
    tbi Latn SD\n\
    tbj Latn PG\n\
    tbk Tagb PH\n\
    tbl Latn PH\n\
    tbm Latn CD\n\
    tbn Latn CO\n\
    tbo Latn PG\n\
    tbp Latn ID\n\
    tbs Latn PG\n\
    tbt Latn CD\n\
    tbu Latn MX\n\
    tbv Latn PG\n\
    tbx Latn PG\n\
    tby Latn ID\n\
    tbz Latn BJ\n\
    tca Latn BR\n\
    tcb Latn US\n\
    tcc Latn TZ\n\
    tcd Latn GH\n\
    tce Latn CA\n\
    tcf Latn MX\n\
    tcg Latn ID\n\
    tch Latn TC\n\
    tci Latn PG\n\
    tck Latn GA\n\
    tcm Latn ID\n\
    tcn Tibt NP\n\
    tco Mymr MM\n\
    tcp Latn MM\n\
    tcq Latn ID\n\
    tcs Latn AU\n\
    tcu Latn MX\n\
    tcw Latn MX\n\
    tcx Taml IN\n\
    tcz Latn IN\n\
    tda Tfng NE\n\
    tdb Deva IN\n\
    tdc Latn CO\n\
    tde Latn ML\n\
    tdi Latn ID\n\
    tdj Latn ID\n\
    tdk Latn NG\n\
    tdl Latn NG\n\
    tdm Latn GY\n\
    tdn Latn ID\n\
    tdo Latn NG\n\
    tdq Latn NG\n\
    tdr Latn VN\n\
    tds Latn ID\n\
    tdt Latn TL\n\
    tdv Latn NG\n\
    tdx Latn MG\n\
    tdy Latn PH\n\
    tea Latn MY\n\
    teb Latn EC\n\
    tec Latn KE\n\
    ted Latn CI\n\
    tee Latn MX\n\
    teg Latn GA\n\
    teh Latn AR\n\
    tei Latn PG\n\
    tek Latn CD\n\
    ten Latn CO\n\
    tep Latn MX\n\
    teq Latn SD\n\
    ter Latn BR\n\
    tes Java ID\n\
    teu Latn UG\n\
    tev Latn ID\n\
    tew Latn US\n\
    tex Latn SS\n\
    tey Latn SD\n\
    tez Latn NE\n\
    tfi Latn BJ\n\
    tfn Latn US\n\
    tfo Latn ID\n\
    tfr Latn PA\n\
    tft Latn ID\n\
    tga Latn KE\n\
    tgb Latn MY\n\
    tgc Latn PG\n\
    tgd Latn NG\n\
    tge Deva NP\n\
    tgf Tibt BT\n\
    tgh Latn TT\n\
    tgi Latn PG\n\
    tgj Latn IN\n\
    tgn Latn PH\n\
    tgo Latn PG\n\
    tgp Latn VU\n\
    tgq Latn MY\n\
    tgs Latn VU\n\
    tgt Latn PH\n\
    tgu Latn PG\n\
    tgv Latn BR\n\
    tgw Latn CI\n\
    tgx Latn CA\n\
    tgy Latn SS\n\
    tgz Latn AU\n\
    thd Latn AU\n\
    the Deva NP\n\
    thf Deva NP\n\
    thh Latn MX\n\
    thi Tale LA\n\
    thk Latn KE\n\
    thm Thai TH\n\
    thp Latn CA\n\
    ths Deva NP\n\
    tht Latn CA\n\
    thu Latn SS\n\
    thv Latn DZ\n\
    thy Latn NG\n\
    thz Latn NE\n\
    tic Latn SD\n\
    tif Latn PG\n\
    tih Latn MY\n\
    tii Latn CD\n\
    tij Deva NP\n\
    tik Latn CM\n\
    til Latn US\n\
    tim Latn PG\n\
    tin Cyrl RU\n\
    tio Latn PG\n\
    tip Latn ID\n\
    tiq Latn BF\n\
    tis Latn PH\n\
    tit Latn CO\n\
    tiu Latn PH\n\
    tiw Latn AU\n\
    tix Latn US\n\
    tiy Latn PH\n\
    tja Latn LR\n\
    tjg Latn ID\n\
    tji Latn CN\n\
    tjj Latn AU\n\
    tjl Mymr MM\n\
    tjn Latn CI\n\
    tjo Arab DZ\n\
    tjp Latn AU\n\
    tjs Latn CN\n\
    tju Latn AU\n\
    tjw Latn AU\n\
    tka Latn BR\n\
    tkb Deva IN\n\
    tkd Latn TL\n\
    tke Latn MZ\n\
    tkf Latn BR\n\
    tkg Latn MG\n\
    tkp Latn SB\n\
    tkq Latn NG\n\
    tks Arab IR\n\
    tku Latn MX\n\
    tkv Latn PG\n\
    tkw Latn SB\n\
    tkx Latn ID\n\
    tkz Latn VN\n\
    tla Latn MX\n\
    tlb Latn ID\n\
    tlc Latn MX\n\
    tld Latn ID\n\
    tlf Latn PG\n\
    tlg Latn ID\n\
    tli Latn US\n\
    tlj Latn UG\n\
    tlk Latn ID\n\
    tll Latn CD\n\
    tlm Latn VU\n\
    tln Latn ID\n\
    tlp Latn MX\n\
    tlq Latn MM\n\
    tlr Latn SB\n\
    tls Latn VU\n\
    tlt Latn ID\n\
    tlu Latn ID\n\
    tlv Latn ID\n\
    tlx Latn PG\n\
    tma Latn TD\n\
    tmb Latn VU\n\
    tmc Latn TD\n\
    tmd Latn PG\n\
    tme Latn BR\n\
    tmf Latn PY\n\
    tmg Latn ID\n\
    tmi Latn VU\n\
    tmj Latn ID\n\
    tml Latn ID\n\
    tmm Latn VN\n\
    tmn Latn ID\n\
    tmo Latn MY\n\
    tmq Latn PG\n\
    tmr Syrc IL\n\
    tmt Latn VU\n\
    tmu Latn ID\n\
    tmv Latn CD\n\
    tmw Latn MY\n\
    tmy Latn PG\n\
    tmz Latn VE\n\
    tna Latn BO\n\
    tnb Latn CO\n\
    tnc Latn CO\n\
    tnd Latn CO\n\
    tng Latn TD\n\
    tnh Latn PG\n\
    tni Latn ID\n\
    tnk Latn VU\n\
    tnl Latn VU\n\
    tnm Latn ID\n\
    tnn Latn VU\n\
    tno Latn BO\n\
    tnp Latn VU\n\
    tnq Latn PR\n\
    tns Latn PG\n\
    tnt Latn ID\n\
    tnv Cakm BD\n\
    tnw Latn ID\n\
    tnx Latn SB\n\
    tny Latn TZ\n\
    tob Latn AR\n\
    toc Latn MX\n\
    tod Latn GN\n\
    tof Latn PG\n\
    toh Latn MZ\n\
    toj Latn MX\n\
    tol Latn US\n\
    tom Latn ID\n\
    too Latn MX\n\
    top Latn MX\n\
    toq Latn SS\n\
    tor Latn CD\n\
    tos Latn MX\n\
    tou Latn VN\n\
    tov Arab IR\n\
    tow Latn US\n\
    tox Latn PW\n\
    toy Latn ID\n\
    toz Latn CM\n\
    tpa Latn PG\n\
    tpc Latn MX\n\
    tpe Latn BD\n\
    tpf Latn ID\n\
    tpg Latn ID\n\
    tpj Latn PY\n\
    tpk Latn BR\n\
    tpl Latn MX\n\
    tpm Latn GH\n\
    tpn Latn BR\n\
    tpp Latn MX\n\
    tpr Latn BR\n\
    tpt Latn MX\n\
    tpu Khmr KH\n\
    tpv Latn MP\n\
    tpx Latn MX\n\
    tpy Latn BR\n\
    tpz Latn PG\n\
    tqb Latn BR\n\
    tql Latn VU\n\
    tqm Latn PG\n\
    tqn Latn US\n\
    tqo Latn PG\n\
    tqp Latn PG\n\
    tqt Latn MX\n\
    tqu Latn SB\n\
    tqw Latn US\n\
    tra Arab AF\n\
    trb Latn PG\n\
    trc Latn MX\n\
    tre Latn ID\n\
    trf Latn TT\n\
    trg Hebr IL\n\
    trh Latn PG\n\
    tri Latn SR\n\
    trj Latn TD\n\
    trl Latn GB\n\
    trm Arab AF\n\
    trn Latn BO\n\
    tro Latn IN\n\
    trp Latn IN\n\
    trq Latn MX\n\
    trr Latn PE\n\
    trs Latn MX\n\
    trt Latn ID\n\
    trx Latn MY\n\
    try Latn IN\n\
    trz Latn BR\n\
    tsa Latn CG\n\
    tsb Latn ET\n\
    tsc Latn MZ\n\
    tsh Latn CM\n\
    tsi Latn CA\n\
    tsl Latn VN\n\
    tsp Latn BF\n\
    tsr Latn VU\n\
    tst Latn ML\n\
    tsu Latn TW\n\
    tsv Latn GA\n\
    tsw Latn NG\n\
    tsx Latn PG\n\
    tsz Latn MX\n\
    ttb Latn NG\n\
    ttc Latn GT\n\
    ttd Latn PG\n\
    tte Latn PG\n\
    ttf Latn CM\n\
    tth Laoo LA\n\
    tti Latn ID\n\
    ttk Latn CO\n\
    ttl Latn ZM\n\
    ttm Latn CA\n\
    ttn Latn ID\n\
    tto Laoo LA\n\
    ttp Latn ID\n\
    ttr Latn NG\n\
    ttu Latn PG\n\
    ttv Latn PG\n\
    ttw Latn MY\n\
    tty Latn ID\n\
    ttz Deva NP\n\
    tua Latn PG\n\
    tub Latn US\n\
    tuc Latn PG\n\
    tud Latn BR\n\
    tue Latn CO\n\
    tuf Latn CO\n\
    tug Latn TD\n\
    tuh Latn PG\n\
    tui Latn CM\n\
    tuj Latn ID\n\
    tul Latn NG\n\
    tun Latn US\n\
    tuo Latn BR\n\
    tuq Latn TD\n\
    tus Latn CA\n\
    tuu Latn US\n\
    tuv Latn KE\n\
    tux Latn BR\n\
    tuy Latn KE\n\
    tuz Latn BF\n\
    tva Latn SB\n\
    tvd Latn NG\n\
    tve Latn ID\n\
    tvi Latn NG\n\
    tvk Latn VU\n\
    tvm Latn ID\n\
    tvn Mymr MM\n\
    tvo Latn ID\n\
    tvs Latn KE\n\
    tvt Latn IN\n\
    tvu Latn CM\n\
    tvw Latn ID\n\
    tvx Latn TW\n\
    twa Latn US\n\
    twb Latn PH\n\
    twd Latn NL\n\
    twe Latn ID\n\
    twf Latn US\n\
    twg Latn ID\n\
    twh Latn VN\n\
    twl Latn MZ\n\
    twm Deva IN\n\
    twn Latn CM\n\
    two Latn BW\n\
    twp Latn PG\n\
    twr Latn MX\n\
    twt Latn BR\n\
    twu Latn ID\n\
    tww Latn PG\n\
    twx Latn MZ\n\
    twy Latn ID\n\
    txa Latn MY\n\
    txe Latn ID\n\
    txi Latn BR\n\
    txj Latn NG\n\
    txm Latn ID\n\
    txn Latn ID\n\
    txq Latn ID\n\
    txs Latn ID\n\
    txt Latn ID\n\
    txu Latn BR\n\
    txx Latn MY\n\
    txy Latn MG\n\
    tya Latn PG\n\
    tye Latn NG\n\
    tyh Latn VN\n\
    tyi Latn CG\n\
    tyj Latn VN\n\
    tyl Latn VN\n\
    tyn Latn ID\n\
    typ Latn AU\n\
    tyr Tavt VN\n\
    tys Latn VN\n\
    tyt Latn VN\n\
    tyu Latn BW\n\
    tyx Latn CG\n\
    tyy Latn NG\n\
    tyz Latn VN\n\
    tzh Latn MX\n\
    tzj Latn GT\n\
    tzl Latn 001\n\
    tzn Latn ID\n\
    tzo Latn MX\n\
    tzx Latn PG\n\
    uam Latn BR\n\
    uar Latn PG\n\
    uba Latn NG\n\
    ubi Latn TD\n\
    ubl Latn PH\n\
    ubr Latn PG\n\
    ubu Latn PG\n\
    uby Latn TR\n\
    uda Latn NG\n\
    ude Cyrl RU\n\
    udg Mlym IN\n\
    udi Cyrl RU\n\
    udj Latn ID\n\
    udl Latn CM\n\
    udu Latn SD\n\
    ues Latn ID\n\
    ufi Latn PG\n\
    ugb Latn AU\n\
    uge Latn SB\n\
    ugh Cyrl RU\n\
    ugo Thai TH\n\
    uha Latn NG\n\
    uhn Latn ID\n\
    uis Latn PG\n\
    uiv Latn CM\n\
    uji Latn NG\n\
    uka Latn ID\n\
    ukg Latn PG\n\
    ukh Latn CF\n\
    uki Orya IN\n\
    ukk Latn MM\n\
    ukp Latn NG\n\
    ukq Latn NG\n\
    uku Latn NG\n\
    ukv Latn SS\n\
    ukw Latn NG\n\
    uky Latn AU\n\
    ula Latn NG\n\
    ulb Latn NG\n\
    ulc Cyrl RU\n\
    ule Latn AR\n\
    ulf Latn ID\n\
    ulk Latn AU\n\
    ulm Latn ID\n\
    uln Latn PG\n\
    ulu Latn ID\n\
    ulw Latn NI\n\
    uly Latn NG\n\
    uma Latn US\n\
    umd Latn AU\n\
    umg Latn AU\n\
    umi Latn MY\n\
    umm Latn NG\n\
    umn Latn MM\n\
    umo Latn BR\n\
    ump Latn AU\n\
    umr Latn AU\n\
    ums Latn ID\n\
    una Latn PG\n\
    une Latn NG\n\
    ung Latn AU\n\
    uni Latn PG\n\
    unk Latn BR\n\
    unm Latn US\n\
    unn Latn AU\n\
    unu Latn PG\n\
    unz Latn ID\n\
    uon Latn TW\n\
    upi Latn PG\n\
    upv Latn VU\n\
    ura Latn PE\n\
    urb Latn BR\n\
    urc Latn AU\n\
    ure Latn BO\n\
    urf Latn AU\n\
    urg Latn PG\n\
    urh Latn NG\n\
    uri Latn PG\n\
    urk Thai TH\n\
    urm Latn PG\n\
    urn Latn ID\n\
    uro Latn PG\n\
    urp Latn BR\n\
    urr Latn VU\n\
    urt Latn PG\n\
    uru Latn BR\n\
    urv Latn PG\n\
    urw Latn PG\n\
    urx Latn PG\n\
    ury Latn ID\n\
    urz Latn BR\n\
    usa Latn PG\n\
    ush Arab PK\n\
    usi Latn BD\n\
    usk Latn CM\n\
    usp Latn GT\n\
    uss Latn NG\n\
    usu Latn PG\n\
    uta Latn NG\n\
    ute Latn US\n\
    uth Latn NG\n\
    utp Latn SB\n\
    utr Latn NG\n\
    utu Latn PG\n\
    uum Grek GE\n\
    uur Latn VU\n\
    uve Latn NC\n\
    uvh Latn PG\n\
    uvl Latn PG\n\
    uwa Latn AU\n\
    uya Latn NG\n\
    uzs Arab AF\n\
    vaa Taml IN\n\
    vae Latn CF\n\
    vaf Arab IR\n\
    vag Latn GH\n\
    vah Deva IN\n\
    vaj Latn NA\n\
    val Latn PG\n\
    vam Latn PG\n\
    van Latn PG\n\
    vao Latn VU\n\
    vap Latn IN\n\
    var Latn MX\n\
    vas Deva IN\n\
    vau Latn CD\n\
    vav Deva IN\n\
    vay Deva NP\n\
    vbb Latn ID\n\
    vbk Latn PH\n\
    vem Latn NG\n\
    veo Latn US\n\
    ver Latn NG\n\
    vgr Arab PK\n\
    vid Latn TZ\n\
    vif Latn CG\n\
    vig Latn BF\n\
    vil Latn AR\n\
    vin Latn TZ\n\
    vit Latn NG\n\
    viv Latn PG\n\
    vjk Deva IN\n\
    vka Latn AU\n\
    vkj Latn TD\n\
    vkk Latn ID\n\
    vkl Latn ID\n\
    vkm Latn BR\n\
    vkn Latn NG\n\
    vko Latn ID\n\
    vkp Latn IN\n\
    vkt Latn ID\n\
    vku Latn AU\n\
    vkz Latn NG\n\
    vlp Latn VU\n\
    vma Latn AU\n\
    vmb Latn AU\n\
    vmc Latn MX\n\
    vmd Knda IN\n\
    vme Latn ID\n\
    vmg Latn PG\n\
    vmh Arab IR\n\
    vmi Latn AU\n\
    vmj Latn MX\n\
    vmk Latn MZ\n\
    vml Latn AU\n\
    vmm Latn MX\n\
    vmp Latn MX\n\
    vmq Latn MX\n\
    vmr Latn MZ\n\
    vms Latn ID\n\
    vmu Latn AU\n\
    vmx Latn MX\n\
    vmy Latn MX\n\
    vmz Latn MX\n\
    vnk Latn SB\n\
    vnm Latn VU\n\
    vnp Latn VU\n\
    vor Latn NG\n\
    vra Latn VU\n\
    vrs Latn SB\n\
    vrt Latn VU\n\
    vto Latn ID\n\
    vum Latn GA\n\
    vut Latn CM\n\
    vwa Latn CN\n\
    waa Latn US\n\
    wab Latn PG\n\
    wac Latn US\n\
    wad Latn ID\n\
    waf Latn BR\n\
    wag Latn PG\n\
    wah Latn ID\n\
    wai Latn ID\n\
    waj Latn PG\n\
    wam Latn US\n\
    wan Latn CI\n\
    wap Latn GY\n\
    waq Latn AU\n\
    was Latn US\n\
    wat Latn PG\n\
    wau Latn BR\n\
    wav Latn NG\n\
    waw Latn BR\n\
    wax Latn PG\n\
    way Latn SR\n\
    waz Latn PG\n\
    wba Latn VE\n\
    wbb Latn ID\n\
    wbe Latn ID\n\
    wbf Latn BF\n\
    wbh Latn TZ\n\
    wbi Latn TZ\n\
    wbj Latn TZ\n\
    wbk Arab AF\n\
    wbl Latn PK\n\
    wbm Latn CN\n\
    wbt Latn AU\n\
    wbv Latn AU\n\
    wbw Latn ID\n\
    wca Latn BR\n\
    wci Latn TG\n\
    wdd Latn GA\n\
    wdg Latn PG\n\
    wdj Latn AU\n\
    wdk Latn AU\n\
    wdt Latn CA\n\
    wdu Latn AU\n\
    wdy Latn AU\n\
    wec Latn CI\n\
    wed Latn PG\n\
    weg Latn AU\n\
    weh Latn CM\n\
    wei Latn PG\n\
    wem Latn BJ\n\
    weo Latn ID\n\
    wep Latn DE\n\
    wer Latn PG\n\
    wes Latn CM\n\
    wet Latn ID\n\
    weu Latn MM\n\
    wew Latn ID\n\
    wfg Latn ID\n\
    wga Latn AU\n\
    wgb Latn PG\n\
    wgg Latn AU\n\
    wgi Latn PG\n\
    wgo Latn ID\n\
    wgu Latn AU\n\
    wgy Latn AU\n\
    wha Latn ID\n\
    whg Latn PG\n\
    whk Latn ID\n\
    whu Latn ID\n\
    wib Latn BF\n\
    wic Latn US\n\
    wie Latn AU\n\
    wif Latn AU\n\
    wig Latn AU\n\
    wih Latn AU\n\
    wii Latn PG\n\
    wij Latn AU\n\
    wik Latn AU\n\
    wil Latn AU\n\
    wim Latn AU\n\
    win Latn US\n\
    wir Latn BR\n\
    wiu Latn PG\n\
    wiv Latn PG\n\
    wiy Latn US\n\
    wja Latn NG\n\
    wji Latn NG\n\
    wka Latn TZ\n\
    wkd Latn ID\n\
    wkr Latn AU\n\
    wkw Latn AU\n\
    wky Latn AU\n\
    wla Latn PG\n\
    wle Ethi ET\n\
    wlg Latn AU\n\
    wlh Latn TL\n\
    wli Latn ID\n\
    wlm Latn GB\n\
    wlo Arab ID\n\
    wlr Latn VU\n\
    wlu Latn AU\n\
    wlv Latn AR\n\
    wlw Latn ID\n\
    wlx Latn GH\n\
    wma Latn NG\n\
    wmb Latn AU\n\
    wmc Latn PG\n\
    wmd Latn BR\n\
    wme Deva NP\n\
    wmh Latn TL\n\
    wmi Latn AU\n\
    wmm Latn ID\n\
    wmn Latn NC\n\
    wmo Latn PG\n\
    wms Latn ID\n\
    wmt Latn AU\n\
    wmw Latn MZ\n\
    wmx Latn PG\n\
    wnb Latn PG\n\
    wnc Latn PG\n\
    wnd Latn AU\n\
    wne Arab PK\n\
    wng Latn ID\n\
    wnk Latn ID\n\
    wnm Latn AU\n\
    wnn Latn AU\n\
    wno Latn ID\n\
    wnp Latn PG\n\
    wnu Latn PG\n\
    wnw Latn US\n\
    wny Latn AU\n\
    woa Latn AU\n\
    wob Latn CI\n\
    woc Latn PG\n\
    wod Latn ID\n\
    woe Latn FM\n\
    wof Latn GM\n\
    wog Latn PG\n\
    woi Latn ID\n\
    wok Latn CM\n\
    wom Latn NG\n\
    won Latn CD\n\
    woo Latn ID\n\
    wor Latn ID\n\
    wos Latn PG\n\
    wow Latn ID\n\
    wpc Latn VE\n\
    wrb Latn AU\n\
    wrg Latn AU\n\
    wrh Latn AU\n\
    wri Latn AU\n\
    wrk Latn AU\n\
    wrl Latn AU\n\
    wrm Latn AU\n\
    wro Latn AU\n\
    wrp Latn ID\n\
    wrr Latn AU\n\
    wrs Latn PG\n\
    wru Latn ID\n\
    wrv Latn PG\n\
    wrw Latn AU\n\
    wrx Latn ID\n\
    wrz Latn AU\n\
    wsa Latn ID\n\
    wsi Latn VU\n\
    wsk Latn PG\n\
    wsr Latn PG\n\
    wss Latn GH\n\
    wsu Latn BR\n\
    wsv Arab AF\n\
    wtb Latn TZ\n\
    wtf Latn PG\n\
    wth Latn AU\n\
    wti Latn ET\n\
    wtk Latn PG\n\
    wtw Latn ID\n\
    wua Latn AU\n\
    wub Latn AU\n\
    wud Latn TG\n\
    wul Latn ID\n\
    wum Latn GA\n\
    wun Latn TZ\n\
    wur Latn AU\n\
    wut Latn PG\n\
    wuv Latn PG\n\
    wux Latn AU\n\
    wuy Latn ID\n\
    wwa Latn BJ\n\
    wwb Latn AU\n\
    wwo Latn VU\n\
    wwr Latn AU\n\
    www Latn CM\n\
    wxw Latn AU\n\
    wyb Latn AU\n\
    wyi Latn AU\n\
    wym Latn PL\n\
    wyn Latn US\n\
    wyr Latn BR\n\
    wyy Latn FJ\n\
    xaa Latn ES\n\
    xab Latn NG\n\
    xai Latn BR\n\
    xaj Latn BR\n\
    xak Latn VE\n\
    xal Cyrl RU\n\
    xam Latn ZA\n\
    xan Ethi ET\n\
    xao Latn VN\n\
    xar Latn PG\n\
    xas Cyrl RU\n\
    xat Latn BR\n\
    xau Latn ID\n\
    xaw Latn US\n\
    xay Latn ID\n\
    xbb Latn AU\n\
    xbd Latn AU\n\
    xbe Latn AU\n\
    xbg Latn AU\n\
    xbi Latn PG\n\
    xbj Latn AU\n\
    xbm Latn FR\n\
    xbn Latn MY\n\
    xbp Latn AU\n\
    xbr Latn ID\n\
    xbw Latn BR\n\
    xby Latn AU\n\
    xch Latn US\n\
    xda Latn AU\n\
    xdk Latn AU\n\
    xdo Latn AO\n\
    xdq Cyrl RU\n\
    xdy Latn ID\n\
    xed Latn CM\n\
    xeg Latn ZA\n\
    xem Latn ID\n\
    xer Latn BR\n\
    xes Latn PG\n\
    xet Latn BR\n\
    xeu Latn PG\n\
    xgb Latn CI\n\
    xgd Latn AU\n\
    xgg Latn AU\n\
    xgi Latn AU\n\
    xgm Latn AU\n\
    xgu Latn AU\n\
    xgw Latn AU\n\
    xhe Arab PK\n\
    xhm Khmr KH\n\
    xhv Latn VN\n\
    xii Latn ZA\n\
    xin Latn GT\n\
    xir Latn BR\n\
    xis Orya IN\n\
    xiy Latn BR\n\
    xjb Latn AU\n\
    xjt Latn AU\n\
    xka Arab PK\n\
    xkb Latn BJ\n\
    xkc Arab IR\n\
    xkd Latn ID\n\
    xke Latn ID\n\
    xkf Tibt BT\n\
    xkg Latn ML\n\
    xkj Arab IR\n\
    xkl Latn ID\n\
    xkn Latn ID\n\
    xkp Arab IR\n\
    xkq Latn ID\n\
    xkr Latn BR\n\
    xks Latn ID\n\
    xkt Latn GH\n\
    xku Latn CG\n\
    xkv Latn BW\n\
    xkw Latn ID\n\
    xkx Latn PG\n\
    xky Latn MY\n\
    xkz Latn BT\n\
    xla Latn PG\n\
    xly Elym IR\n\
    xma Latn SO\n\
    xmb Latn CM\n\
    xmc Latn MZ\n\
    xmd Latn CM\n\
    xmg Latn CM\n\
    xmh Latn AU\n\
    xmj Latn CM\n\
    xmm Latn ID\n\
    xmo Latn BR\n\
    xmp Latn AU\n\
    xmq Latn AU\n\
    xmt Latn ID\n\
    xmu Latn AU\n\
    xmv Latn MG\n\
    xmw Latn MG\n\
    xmx Latn ID\n\
    xmy Latn AU\n\
    xmz Latn ID\n\
    xnb Latn TW\n\
    xni Latn AU\n\
    xnj Latn TZ\n\
    xnk Latn AU\n\
    xnm Latn AU\n\
    xnn Latn PH\n\
    xnq Latn MZ\n\
    xnt Latn US\n\
    xnu Latn AU\n\
    xny Latn AU\n\
    xnz Latn EG\n\
    xoc Latn NG\n\
    xod Latn ID\n\
    xoi Latn PG\n\
    xok Latn BR\n\
    xom Latn SD\n\
    xon Latn GH\n\
    xoo Latn BR\n\
    xop Latn PG\n\
    xor Latn BR\n\
    xow Latn PG\n\
    xpa Latn AU\n\
    xpb Latn AU\n\
    xpd Latn AU\n\
    xpf Latn AU\n\
    xpg Grek TR\n\
    xph Latn AU\n\
    xpi Ogam GB\n\
    xpj Latn AU\n\
    xpk Latn BR\n\
    xpl Latn AU\n\
    xpm Cyrl RU\n\
    xpn Latn BR\n\
    xpo Latn MX\n\
    xpq Latn US\n\
    xpt Latn AU\n\
    xpv Latn AU\n\
    xpw Latn AU\n\
    xpx Latn AU\n\
    xpz Latn AU\n\
    xra Latn BR\n\
    xrb Latn BF\n\
    xrd Latn AU\n\
    xre Latn BR\n\
    xrg Latn AU\n\
    xri Latn BR\n\
    xrm Cyrl RU\n\
    xrn Cyrl RU\n\
    xrr Latn IT\n\
    xru Latn AU\n\
    xrw Latn PG\n\
    xsb Latn PH\n\
    xse Latn ID\n\
    xsh Latn NG\n\
    xsi Latn PG\n\
    xsm Latn GH\n\
    xsn Latn NG\n\
    xsp Latn PG\n\
    xsq Latn MZ\n\
    xsu Latn VE\n\
    xsy Latn TW\n\
    xta Latn MX\n\
    xtb Latn MX\n\
    xtc Latn SD\n\
    xtd Latn MX\n\
    xte Latn ID\n\
    xth Latn AU\n\
    xti Latn MX\n\
    xtj Latn MX\n\
    xtl Latn MX\n\
    xtm Latn MX\n\
    xtn Latn MX\n\
    xtp Latn MX\n\
    xtq Brah IR\n\
    xts Latn MX\n\
    xtt Latn MX\n\
    xtu Latn MX\n\
    xtv Latn AU\n\
    xtw Latn BR\n\
    xty Latn MX\n\
    xub Taml IN\n\
    xud Latn AU\n\
    xuj Taml IN\n\
    xul Latn AU\n\
    xum Latn IT\n\
    xun Latn AU\n\
    xuo Latn TD\n\
    xut Latn AU\n\
    xuu Latn NA\n\
    xve Ital IT\n\
    xvi Arab AF\n\
    xvn Latn ES\n\
    xvo Latn IT\n\
    xvs Latn IT\n\
    xwa Latn BR\n\
    xwd Latn AU\n\
    xwe Latn BJ\n\
    xwj Latn AU\n\
    xwk Latn AU\n\
    xwl Latn BJ\n\
    xwo Cyrl RU\n\
    xwr Latn ID\n\
    xwt Latn AU\n\
    xww Latn AU\n\
    xxb Latn GH\n\
    xxk Latn ID\n\
    xxm Latn AU\n\
    xxr Latn BR\n\
    xxt Latn ID\n\
    xya Latn AU\n\
    xyb Latn AU\n\
    xyj Latn AU\n\
    xyk Latn AU\n\
    xyl Latn BR\n\
    xyt Latn AU\n\
    xyy Latn AU\n\
    xzh Marc CN\n\
    xzp Latn MX\n\
    yaa Latn PE\n\
    yab Latn BR\n\
    yac Latn ID\n\
    yad Latn PE\n\
    yae Latn VE\n\
    yaf Latn CD\n\
    yag Latn CL\n\
    yah Latn TJ\n\
    yai Cyrl TJ\n\
    yaj Latn CF\n\
    yak Latn US\n\
    yal Latn GN\n\
    yam Latn CM\n\
    yan Latn NI\n\
    yaq Latn MX\n\
    yar Latn VE\n\
    yas Latn CM\n\
    yat Latn CM\n\
    yau Latn VE\n\
    yaw Latn BR\n\
    yax Latn AO\n\
    yay Latn NG\n\
    yaz Latn NG\n\
    yba Latn NG\n\
    ybe Latn CN\n\
    ybh Deva NP\n\
    ybi Deva NP\n\
    ybj Latn NG\n\
    ybl Latn NG\n\
    ybm Latn PG\n\
    ybn Latn BR\n\
    ybo Latn PG\n\
    ybx Latn PG\n\
    yby Latn PG\n\
    ycl Latn CN\n\
    ycn Latn CO\n\
    ycr Latn TW\n\
    yda Latn AU\n\
    yde Latn PG\n\
    ydg Arab PK\n\
    ydk Latn PG\n\
    yea Mlym IN\n\
    yec Latn DE\n\
    yee Latn PG\n\
    yei Latn CM\n\
    yej Grek GR\n\
    yel Latn CD\n\
    yer Latn NG\n\
    yes Latn NG\n\
    yet Latn ID\n\
    yeu Telu IN\n\
    yev Latn PG\n\
    yey Latn BW\n\
    yga Latn AU\n\
    ygi Latn AU\n\
    ygl Latn PG\n\
    ygm Latn PG\n\
    ygp Plrd CN\n\
    ygr Latn PG\n\
    ygu Latn AU\n\
    ygw Latn PG\n\
    yhd Hebr IL\n\
    yia Latn AU\n\
    yig Yiii CN\n\
    yih Hebr DE\n\
    yii Latn AU\n\
    yij Latn AU\n\
    yil Latn AU\n\
    yim Latn IN\n\
    yir Latn ID\n\
    yis Latn PG\n\
    yiv Yiii CN\n\
    yka Latn PH\n\
    ykg Cyrl RU\n\
    ykh Cyrl MN\n\
    yki Latn ID\n\
    ykk Latn PG\n\
    ykm Latn PG\n\
    yko Latn CM\n\
    ykr Latn PG\n\
    yky Latn CF\n\
    yla Latn PG\n\
    ylb Latn PG\n\
    yle Latn PG\n\
    ylg Latn PG\n\
    yli Latn ID\n\
    yll Latn PG\n\
    ylr Latn AU\n\
    ylu Latn PG\n\
    yly Latn NC\n\
    ymb Latn PG\n\
    yme Latn PE\n\
    ymg Latn CD\n\
    ymk Latn MZ\n\
    yml Latn PG\n\
    ymm Latn SO\n\
    ymn Latn ID\n\
    ymo Latn PG\n\
    ymp Latn PG\n\
    yna Plrd CN\n\
    ynd Latn AU\n\
    yng Latn CD\n\
    ynk Cyrl RU\n\
    ynl Latn PG\n\
    ynq Latn NG\n\
    yns Latn CD\n\
    ynu Latn CO\n\
    yob Latn PG\n\
    yog Latn PH\n\
    yoi Jpan JP\n\
    yok Latn US\n\
    yol Latn IE\n\
    yom Latn CD\n\
    yon Latn PG\n\
    yot Latn NG\n\
    yoy Thai TH\n\
    yra Latn PG\n\
    yrb Latn PG\n\
    yre Latn CI\n\
    yrk Cyrl RU\n\
    yrm Latn AU\n\
    yro Latn BR\n\
    yrs Latn ID\n\
    yrw Latn PG\n\
    yry Latn AU\n\
    ysd Yiii CN\n\
    ysn Yiii CN\n\
    ysp Yiii CN\n\
    ysr Cyrl RU\n\
    yss Latn PG\n\
    ysy Plrd CN\n\
    ytw Latn PG\n\
    yty Latn AU\n\
    yub Latn AU\n\
    yuc Latn US\n\
    yud Hebr IL\n\
    yuf Latn US\n\
    yug Cyrl RU\n\
    yui Latn CO\n\
    yuj Latn PG\n\
    yul Latn CF\n\
    yum Latn US\n\
    yun Latn NG\n\
    yup Latn CO\n\
    yuq Latn BO\n\
    yur Latn US\n\
    yut Latn PG\n\
    yuw Latn PG\n\
    yux Cyrl RU\n\
    yuz Latn BO\n\
    yva Latn ID\n\
    yvt Latn VE\n\
    ywa Latn PG\n\
    ywg Latn AU\n\
    ywn Latn BR\n\
    ywq Plrd CN\n\
    ywr Latn AU\n\
    ywu Plrd CN\n\
    yww Latn AU\n\
    yxa Latn AU\n\
    yxg Latn AU\n\
    yxl Latn AU\n\
    yxm Latn AU\n\
    yxu Latn AU\n\
    yxy Latn AU\n\
    yyr Latn AU\n\
    yyu Latn PG\n\
    zaa Latn MX\n\
    zab Latn MX\n\
    zac Latn MX\n\
    zad Latn MX\n\
    zae Latn MX\n\
    zaf Latn MX\n\
    zah Latn NG\n\
    zaj Latn TZ\n\
    zak Latn TZ\n\
    zam Latn MX\n\
    zao Latn MX\n\
    zap Latn MX\n\
    zaq Latn MX\n\
    zar Latn MX\n\
    zas Latn MX\n\
    zat Latn MX\n\
    zau Tibt IN\n\
    zav Latn MX\n\
    zaw Latn MX\n\
    zax Latn MX\n\
    zay Latn ET\n\
    zaz Latn NG\n\
    zba Arab 001\n\
    zbc Latn MY\n\
    zbe Latn MY\n\
    zbt Latn ID\n\
    zbu Latn NG\n\
    zbw Latn MY\n\
    zca Latn MX\n\
    zch Hani CN\n\
    zeg Latn PG\n\
    zeh Hani CN\n\
    zem Latn NG\n\
    zen Tfng MR\n\
    zga Latn TZ\n\
    zgb Hani CN\n\
    zgm Hani CN\n\
    zgn Hani CN\n\
    zgr Latn PG\n\
    zhd Hani CN\n\
    zhi Latn NG\n\
    zhn Latn CN\n\
    zhw Latn CM\n\
    zia Latn PG\n\
    zik Latn PG\n\
    zil Latn GN\n\
    zim Latn TD\n\
    zin Latn TZ\n\
    ziw Latn TZ\n\
    ziz Latn NG\n\
    zka Latn ID\n\
    zkd Latn MM\n\
    zko Cyrl RU\n\
    zkp Latn BR\n\
    zku Latn AU\n\
    zkz Cyrl RU\n\
    zla Latn CD\n\
    zlj Hani CN\n\
    zln Hani CN\n\
    zlq Hani CN\n\
    zlu Latn NG\n\
    zma Latn AU\n\
    zmb Latn CD\n\
    zmc Latn AU\n\
    zmd Latn AU\n\
    zme Latn AU\n\
    zmf Latn CD\n\
    zmg Latn AU\n\
    zmh Latn PG\n\
    zmj Latn AU\n\
    zmk Latn AU\n\
    zml Latn AU\n\
    zmm Latn AU\n\
    zmn Latn GA\n\
    zmo Latn SD\n\
    zmp Latn CD\n\
    zmq Latn CD\n\
    zmr Latn AU\n\
    zms Latn CD\n\
    zmt Latn AU\n\
    zmu Latn AU\n\
    zmv Latn AU\n\
    zmw Latn CD\n\
    zmx Latn CG\n\
    zmy Latn AU\n\
    zmz Latn CD\n\
    zna Latn TD\n\
    zne Latn CD\n\
    zng Latn VN\n\
    znk Latn AU\n\
    zns Latn NG\n\
    zoc Latn MX\n\
    zoh Latn MX\n\
    zom Latn IN\n\
    zoo Latn MX\n\
    zoq Latn MX\n\
    zor Latn MX\n\
    zos Latn MX\n\
    zpa Latn MX\n\
    zpb Latn MX\n\
    zpc Latn MX\n\
    zpd Latn MX\n\
    zpe Latn MX\n\
    zpf Latn MX\n\
    zpg Latn MX\n\
    zph Latn MX\n\
    zpi Latn MX\n\
    zpj Latn MX\n\
    zpk Latn MX\n\
    zpl Latn MX\n\
    zpm Latn MX\n\
    zpn Latn MX\n\
    zpo Latn MX\n\
    zpp Latn MX\n\
    zpq Latn MX\n\
    zpr Latn MX\n\
    zps Latn MX\n\
    zpt Latn MX\n\
    zpu Latn MX\n\
    zpv Latn MX\n\
    zpw Latn MX\n\
    zpx Latn MX\n\
    zpy Latn MX\n\
    zpz Latn MX\n\
    zqe Hani CN\n\
    zrg Orya IN\n\
    zrn Latn TD\n\
    zro Latn EC\n\
    zrp Hebr FR\n\
    zrs Latn ID\n\
    zsa Latn PG\n\
    zsr Latn MX\n\
    zsu Latn PG\n\
    zte Latn MX\n\
    ztg Latn MX\n\
    ztl Latn MX\n\
    ztm Latn MX\n\
    ztn Latn MX\n\
    ztp Latn MX\n\
    ztq Latn MX\n\
    zts Latn MX\n\
    ztt Latn MX\n\
    ztu Latn MX\n\
    ztx Latn MX\n\
    zty Latn MX\n\
    zuh Latn PG\n\
    zum Arab OM\n\
    zun Latn US\n\
    zuy Latn CM\n\
    zwa Ethi ET\n\
    zyg Hani CN\n\
    zyj Latn CN\n\
    zyn Hani CN\n\
    zyp Latn MM\n\
    zzj Hani CN\n\
    ";

/// `<likelySubtag from="L_S" to="L_S_R"/>` as `L S R`.
pub(crate) static LIKELY_LANG_SCRIPT: &str = "\
    arc Hatr IQ\n\
    arc Nbat JO\n\
    arc Palm SY\n\
    az Arab IR\n\
    bap Krai IN\n\
    cu Glag BG\n\
    en Shaw GB\n\
    ff Adlm GN\n\
    hak Hant TW\n\
    hnj Hmng LA\n\
    kk Arab CN\n\
    ku Arab IQ\n\
    ku Yezi GE\n\
    ky Arab CN\n\
    ky Latn TR\n\
    lif Limb IN\n\
    lzz Geor GE\n\
    man Nkoo GN\n\
    mn Mong CN\n\
    nan Hant TW\n\
    pa Arab PK\n\
    pal Phlp CN\n\
    pnt Cyrl RU\n\
    pnt Latn TR\n\
    sd Deva IN\n\
    sd Khoj IN\n\
    sd Sind IN\n\
    tg Arab PK\n\
    ug Cyrl KZ\n\
    unr Deva NP\n\
    uz Arab AF\n\
    yue Hans CN\n\
    zh Bopo TW\n\
    zh Hanb TW\n\
    zh Hant TW\n\
    ";

/// `<likelySubtag from="L_R" to="L_S_R"/>` as `L R S`.
pub(crate) static LIKELY_LANG_REGION: &str = "\
    az IQ Arab\n\
    az IR Arab\n\
    az RU Cyrl\n\
    ha CM Arab\n\
    ha SD Arab\n\
    hak TW Hant\n\
    kk AF Arab\n\
    kk CN Arab\n\
    kk IR Arab\n\
    kk MN Arab\n\
    ku LB Arab\n\
    ky CN Arab\n\
    ky TR Latn\n\
    lzz GE Geor\n\
    mn CN Mong\n\
    ms CC Arab\n\
    nan TW Hant\n\
    pa PK Arab\n\
    pnt RU Cyrl\n\
    pnt TR Latn\n\
    sd IN Deva\n\
    sr ME Latn\n\
    sr RO Latn\n\
    sr TR Latn\n\
    tg PK Arab\n\
    ug KZ Cyrl\n\
    ug MN Cyrl\n\
    unr NP Deva\n\
    uz AF Arab\n\
    uz CN Cyrl\n\
    yue CN Hans\n\
    zh AU Hant\n\
    zh BN Hant\n\
    zh GB Hant\n\
    zh GF Hant\n\
    zh HK Hant\n\
    zh ID Hant\n\
    zh MO Hant\n\
    zh PA Hant\n\
    zh PF Hant\n\
    zh PH Hant\n\
    zh SR Hant\n\
    zh TH Hant\n\
    zh TW Hant\n\
    zh US Hant\n\
    zh VN Hant\n\
    ";

/// `<likelySubtag from="und_S" to="L_S_R"/>` as `S L R`.
pub(crate) static LIKELY_UND_SCRIPT: &str = "\
    Adlm ff GN\n\
    Aghb xag AZ\n\
    Ahom aho IN\n\
    Arab ar EG\n\
    Armi arc IR\n\
    Armn hy AM\n\
    Avst ae IR\n\
    Bali ban ID\n\
    Bamu bax CM\n\
    Bass bsq LR\n\
    Batk bbc ID\n\
    Beng bn BD\n\
    Bhks sa IN\n\
    Bopo zh TW\n\
    Brah pka IN\n\
    Brai fr FR\n\
    Bugi bug ID\n\
    Buhd bku PH\n\
    Cakm ccp BD\n\
    Cans iu CA\n\
    Cari xcr TR\n\
    Cham cjm VN\n\
    Cher chr US\n\
    Chrs xco UZ\n\
    Copt cop EG\n\
    Cpmn und CY\n\
    Cprt ecy CY\n\
    Cyrl ru RU\n\
    Deva hi IN\n\
    Diak dv MV\n\
    Dogr doi IN\n\
    Dupl fr FR\n\
    Egyp egy EG\n\
    Elba sq AL\n\
    Elym arc IR\n\
    Ethi am ET\n\
    Gara wo SN\n\
    Geor ka GE\n\
    Glag cu BG\n\
    Gong wsg IN\n\
    Gonm esg IN\n\
    Goth got UA\n\
    Gran sa IN\n\
    Grek el GR\n\
    Gujr gu IN\n\
    Gukh gvr NP\n\
    Guru pa IN\n\
    Hanb zh TW\n\
    Hang ko KR\n\
    Hani zh CN\n\
    Hano hnn PH\n\
    Hans zh CN\n\
    Hant zh TW\n\
    Hatr arc IQ\n\
    Hebr he IL\n\
    Hira ja JP\n\
    Hluw hlu TR\n\
    Hmng hnj LA\n\
    Hmnp hnj US\n\
    Hung hu HU\n\
    Ital ett IT\n\
    Jamo ko KR\n\
    Java jv ID\n\
    Jpan ja JP\n\
    Kali eky MM\n\
    Kana ja JP\n\
    Kawi kaw ID\n\
    Khar pra PK\n\
    Khmr km KH\n\
    Khoj sd IN\n\
    Kits zkt CN\n\
    Knda kn IN\n\
    Kore ko KR\n\
    Krai bap IN\n\
    Kthi bho IN\n\
    Lana nod TH\n\
    Laoo lo LA\n\
    Lepc lep IN\n\
    Limb lif IN\n\
    Lina lab GR\n\
    Linb gmy GR\n\
    Lisu lis CN\n\
    Lyci xlc TR\n\
    Lydi xld TR\n\
    Mahj hi IN\n\
    Maka mak ID\n\
    Mand myz IR\n\
    Mani xmn CN\n\
    Marc bo CN\n\
    Medf dmf NG\n\
    Mend men SL\n\
    Merc xmr SD\n\
    Mero xmr SD\n\
    Mlym ml IN\n\
    Modi mr IN\n\
    Mong mn CN\n\
    Mroo mro BD\n\
    Mtei mni IN\n\
    Mult skr PK\n\
    Mymr my MM\n\
    Nagm unr IN\n\
    Nand sa IN\n\
    Narb xna SA\n\
    Nbat arc JO\n\
    Newa new NP\n\
    Nkoo man GN\n\
    Nshu zhx CN\n\
    Ogam sga IE\n\
    Olck sat IN\n\
    Onao unr IN\n\
    Orkh otk MN\n\
    Orya or IN\n\
    Osge osa US\n\
    Osma so SO\n\
    Ougr oui CN\n\
    Palm arc SY\n\
    Pauc ctd MM\n\
    Perm kv RU\n\
    Phag lzh CN\n\
    Phli pal IR\n\
    Phlp pal CN\n\
    Phnx phn LB\n\
    Plrd hmd CN\n\
    Prti xpr IR\n\
    Rjng rej ID\n\
    Rohg rhg MM\n\
    Runr non SE\n\
    Samr smp IL\n\
    Sarb xsa YE\n\
    Saur saz IN\n\
    Sgnw ase US\n\
    Shaw en GB\n\
    Shrd sa IN\n\
    Sidd sa IN\n\
    Sind sd IN\n\
    Sinh si LK\n\
    Sogd sog UZ\n\
    Sogo sog UZ\n\
    Sora srb IN\n\
    Soyo cmg MN\n\
    Sund su ID\n\
    Sunu suz NP\n\
    Sylo syl BD\n\
    Syrc syr IQ\n\
    Tagb tbw PH\n\
    Takr doi IN\n\
    Tale tdd CN\n\
    Talu khb CN\n\
    Taml ta IN\n\
    Tang txg CN\n\
    Tavt blt VN\n\
    Telu te IN\n\
    Tfng zgh MA\n\
    Tglg fil PH\n\
    Thaa dv MV\n\
    Thai th TH\n\
    Tibt bo CN\n\
    Tirh mai IN\n\
    Tnsa nst IN\n\
    Todr sq AL\n\
    Toto txo IN\n\
    Tutg sa IN\n\
    Ugar uga SY\n\
    Vaii vai LR\n\
    Vith sq AL\n\
    Wara hoc IN\n\
    Wcho nnp IN\n\
    Xpeo peo IR\n\
    Xsux akk IQ\n\
    Yezi ku GE\n\
    Yiii ii CN\n\
    Zanb cmg MN\n\
    ";

/// `<likelySubtag from="und_R" to="L_S_R"/>` as `R L S`.
pub(crate) static LIKELY_UND_REGION: &str = "\
    419 es Latn\n\
    AD ca Latn\n\
    AE ar Arab\n\
    AF fa Arab\n\
    AL sq Latn\n\
    AM hy Armn\n\
    AO pt Latn\n\
    AR es Latn\n\
    AS sm Latn\n\
    AT de Latn\n\
    AW nl Latn\n\
    AX sv Latn\n\
    AZ az Latn\n\
    BA bs Latn\n\
    BD bn Beng\n\
    BE nl Latn\n\
    BF fr Latn\n\
    BG bg Cyrl\n\
    BH ar Arab\n\
    BI rn Latn\n\
    BJ fr Latn\n\
    BL fr Latn\n\
    BN ms Latn\n\
    BO es Latn\n\
    BQ pap Latn\n\
    BR pt Latn\n\
    BT dz Tibt\n\
    BV no Latn\n\
    BY be Cyrl\n\
    CC ms Arab\n\
    CD fr Latn\n\
    CF sg Latn\n\
    CG fr Latn\n\
    CH de Latn\n\
    CI fr Latn\n\
    CL es Latn\n\
    CM fr Latn\n\
    CN zh Hans\n\
    CO es Latn\n\
    CR es Latn\n\
    CU es Latn\n\
    CV pt Latn\n\
    CW pap Latn\n\
    CY el Grek\n\
    CZ cs Latn\n\
    DE de Latn\n\
    DJ fr Latn\n\
    DK da Latn\n\
    DO es Latn\n\
    DZ ar Arab\n\
    EA es Latn\n\
    EC es Latn\n\
    EE et Latn\n\
    EG ar Arab\n\
    EH ar Arab\n\
    ER ti Ethi\n\
    ES es Latn\n\
    ET am Ethi\n\
    FI fi Latn\n\
    FO fo Latn\n\
    FR fr Latn\n\
    GA fr Latn\n\
    GE ka Geor\n\
    GF fr Latn\n\
    GH ak Latn\n\
    GL kl Latn\n\
    GN fr Latn\n\
    GP fr Latn\n\
    GQ es Latn\n\
    GR el Grek\n\
    GT es Latn\n\
    GW pt Latn\n\
    HK zh Hant\n\
    HN es Latn\n\
    HR hr Latn\n\
    HT ht Latn\n\
    HU hu Latn\n\
    IC es Latn\n\
    ID id Latn\n\
    IL he Hebr\n\
    IN hi Deva\n\
    IQ ar Arab\n\
    IR fa Arab\n\
    IS is Latn\n\
    IT it Latn\n\
    JO ar Arab\n\
    JP ja Jpan\n\
    KE sw Latn\n\
    KG ky Cyrl\n\
    KH km Khmr\n\
    KM ar Arab\n\
    KP ko Kore\n\
    KR ko Kore\n\
    KW ar Arab\n\
    KZ ru Cyrl\n\
    LA lo Laoo\n\
    LB ar Arab\n\
    LI de Latn\n\
    LK si Sinh\n\
    LS st Latn\n\
    LT lt Latn\n\
    LU fr Latn\n\
    LV lv Latn\n\
    LY ar Arab\n\
    MA ar Arab\n\
    MC fr Latn\n\
    MD ro Latn\n\
    ME sr Latn\n\
    MF fr Latn\n\
    MG mg Latn\n\
    MK mk Cyrl\n\
    ML bm Latn\n\
    MM my Mymr\n\
    MN mn Cyrl\n\
    MO zh Hant\n\
    MQ fr Latn\n\
    MR ar Arab\n\
    MT mt Latn\n\
    MU fr Latn\n\
    MV dv Thaa\n\
    MX es Latn\n\
    MY ms Latn\n\
    MZ pt Latn\n\
    NA af Latn\n\
    NC fr Latn\n\
    NE ha Latn\n\
    NI es Latn\n\
    NL nl Latn\n\
    NO nb Latn\n\
    NP ne Deva\n\
    OM ar Arab\n\
    PA es Latn\n\
    PE es Latn\n\
    PF fr Latn\n\
    PG tpi Latn\n\
    PH fil Latn\n\
    PK ur Arab\n\
    PL pl Latn\n\
    PM fr Latn\n\
    PR es Latn\n\
    PS ar Arab\n\
    PT pt Latn\n\
    PW pau Latn\n\
    PY gn Latn\n\
    QA ar Arab\n\
    RE fr Latn\n\
    RO ro Latn\n\
    RS sr Cyrl\n\
    RU ru Cyrl\n\
    RW rw Latn\n\
    SA ar Arab\n\
    SC fr Latn\n\
    SD ar Arab\n\
    SE sv Latn\n\
    SI sl Latn\n\
    SJ nb Latn\n\
    SK sk Latn\n\
    SM it Latn\n\
    SN wo Latn\n\
    SO so Latn\n\
    SR nl Latn\n\
    SS ar Arab\n\
    ST pt Latn\n\
    SV es Latn\n\
    SY ar Arab\n\
    TD ar Arab\n\
    TF fr Latn\n\
    TG fr Latn\n\
    TH th Thai\n\
    TJ tg Cyrl\n\
    TK tkl Latn\n\
    TL pt Latn\n\
    TM tk Latn\n\
    TN ar Arab\n\
    TO to Latn\n\
    TR tr Latn\n\
    TV tvl Latn\n\
    TW zh Hant\n\
    TZ sw Latn\n\
    UA uk Cyrl\n\
    UG sw Latn\n\
    UY es Latn\n\
    UZ uz Latn\n\
    VA it Latn\n\
    VE es Latn\n\
    VN vi Latn\n\
    VU bi Latn\n\
    WF fr Latn\n\
    WS sm Latn\n\
    XK sq Latn\n\
    YE ar Arab\n\
    YT fr Latn\n\
    ZW sn Latn\n\
    ";

/// `<likelySubtag from="und_S_R" to="L_S_R"/>` as `S R L`.
pub(crate) static LIKELY_UND_SCRIPT_REGION: &str = "\
    Arab AF fa\n\
    Arab AZ az\n\
    Arab BN ms\n\
    Arab CC ms\n\
    Arab CN ug\n\
    Arab GB ur\n\
    Arab ID ms\n\
    Arab IN ur\n\
    Arab IR fa\n\
    Arab KH cja\n\
    Arab MM rhg\n\
    Arab MN kk\n\
    Arab MU ur\n\
    Arab NG ha\n\
    Arab PK ur\n\
    Arab TH mfa\n\
    Arab TJ fa\n\
    Arab TR apc\n\
    Arab YT swb\n\
    Cyrl AF kaa\n\
    Cyrl AL mk\n\
    Cyrl AZ az\n\
    Cyrl BA sr\n\
    Cyrl BG bg\n\
    Cyrl BY be\n\
    Cyrl GE ab\n\
    Cyrl GR mk\n\
    Cyrl IR kaa\n\
    Cyrl KG ky\n\
    Cyrl MD uk\n\
    Cyrl ME sr\n\
    Cyrl MK mk\n\
    Cyrl MN mn\n\
    Cyrl RO bg\n\
    Cyrl RS sr\n\
    Cyrl SK uk\n\
    Cyrl TJ tg\n\
    Cyrl TR kbd\n\
    Cyrl UA uk\n\
    Cyrl UZ uz\n\
    Cyrl XK sr\n\
    Deva BT ne\n\
    Deva FJ hif\n\
    Deva MU bho\n\
    Deva NP ne\n\
    Deva PK btv\n\
    Ethi ER ti\n\
    Grek TR bgx\n\
    Hant CA yue\n\
    Hant CN yue\n\
    Hebr SE yi\n\
    Hebr UA yi\n\
    Hebr US yi\n\
    Latn AE en\n\
    Latn AF tk\n\
    Latn AM ku\n\
    Latn BD en\n\
    Latn BG en\n\
    Latn BT en\n\
    Latn CC en\n\
    Latn CN za\n\
    Latn CY tr\n\
    Latn DZ fr\n\
    Latn EG en\n\
    Latn ER en\n\
    Latn ET en\n\
    Latn GE ku\n\
    Latn GR en\n\
    Latn HK en\n\
    Latn IL en\n\
    Latn IN en\n\
    Latn IQ en\n\
    Latn IR tk\n\
    Latn JO en\n\
    Latn KM fr\n\
    Latn KZ en\n\
    Latn LB en\n\
    Latn LK en\n\
    Latn MA fr\n\
    Latn MK sq\n\
    Latn MM kac\n\
    Latn MO en\n\
    Latn MR fr\n\
    Latn MV en\n\
    Latn NP en\n\
    Latn PK en\n\
    Latn RU krl\n\
    Latn SD en\n\
    Latn SS en\n\
    Latn SY ku\n\
    Latn TD fr\n\
    Latn TH en\n\
    Latn TN fr\n\
    Latn TW trv\n\
    Latn UA pl\n\
    Latn YE en\n\
    Mymr IN kht\n\
    Mymr TH mnw\n\
    Nkoo ML bm\n\
    Thai CN lcp\n\
    Thai KH kdt\n\
    Thai LA kdt\n\
    Tibt BT dz\n\
    ";
