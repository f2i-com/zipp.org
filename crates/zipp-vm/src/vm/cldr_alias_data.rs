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

/// `<likelySubtag from="L" to="L_S_R"/>`, packed. The languages: three
/// bytes each, space-padded, sorted bytewise. Entry `i`'s script and region
/// are `LIKELY_LANG_SCRIPTS_AT[i]` and `LIKELY_LANG_REGIONS_AT[i]`, indices
/// into the two name lists below. The target language always repeats the
/// source, so it is not stored. See `cldr_alias::likely_lang`.
pub(crate) static LIKELY_LANG_KEYS: &str = "\
    aa aaaaabaacaadaaeaafaagaahaaiaakaalaanaaoaapaaqaasaataauaawaaxaazab aba\
    abbabcabdabeabfabgabhabiablabmabnaboabpabrabsabtabuabvabwabxabyabzacaacb\
    acdaceacfachacmacnacpacqacracsactacuacvacwacxacyaczadaadbaddadeadfadgadh\
    adiadjadladnadoadqadradtaduadwadxadyadzae aeaaebaecaeeaekaelaemaeqaeraeu\
    aewaeyaezaf afbafdafeafhafiafkafnafoafpafsafuafzagaagbagcagdageagfaggagh\
    agiagjagkaglagmagnagoagqagragsagtaguagvagwagxagyagzahaahbahgahhahiahkahl\
    ahmahnahoahpahrahsahtaiaaibaicaidaieaifaigaiiaijaikailaimainaioaipaiqair\
    aitaiwaixaiyajaajgajiajnajwajzak akbakcakdakeakfakgakhakiakkaklakoakpakq\
    akraksaktakuakvakwakzalaalcaldalealfalhalialjalkallalmalnaloalpalqalralt\
    alualwalxalyalzam amaambamcameamfamgamiamjamkammamnamoampamqamramsamtamu\
    amvamwamxamyamzan anaanbancandaneanfanganhanianjankanlanmannanoanpanqanr\
    ansantanuanvanwanxanyanzaoaaobaocaodaoeaofaogaoiaojaokaolaomaonaoraosaot\
    aoxaozapbapcapdapeapfapgaphapiapjapkaplapmapnapoappaprapsaptapuapvapwapx\
    apyapzaqcaqdaqgaqkaqmaqnaqraqtaqzar arcardarearhariarjarkarlarnaroarparq\
    arrarsaruarwarxaryarzas asaasbascaseasgashasiasjaskaslasnasoasrassastasu\
    asvasxasyaszataatbatcatdateatgatiatjatkatlatmatnatoatpatqatratsattatuatv\
    atwatxatyatzauaaucaudaugauhauiaujaukaulaumaunauoaupauqaurautauuauwauyauz\
    av avbavdaviavkavlavmavnavoavsavtavuavvawaawbawcaweawgawhawiawkawmawnawo\
    awrawsawtawuawvawwawxawyaxbaxeaxgaxkaxlaxmaxxay ayaaybaycaydayeaygayhayi\
    aykaylaynayoaypayqaysaytayuayzaz azbazdazgazmaznazoaztazzba baababbacbae\
    bafbagbahbajbalbanbaobapbarbasbaubavbawbaxbaybbabbbbbcbbdbbebbfbbgbbibbj\
    bbkbblbbmbbnbbobbpbbqbbrbbsbbtbbubbvbbwbbxbbybcabcbbcdbcebcfbcgbchbcibcj\
    bckbcmbcnbcobcpbcqbcrbcsbctbcubcvbcwbcybczbdabdbbdcbddbdebdfbdgbdhbdibdj\
    bdkbdlbdmbdnbdobdpbdqbdrbdsbdtbdubdvbdwbdxbdybdzbe beabebbecbedbeebefbeh\
    beibejbekbembeobepbeqbesbetbeubevbewbexbeybezbfabfbbfcbfdbfebffbfgbfhbfj\
    bflbfmbfnbfobfpbfqbfsbftbfubfwbfxbfybfzbg bgabgbbgcbgdbgfbggbgibgjbgnbgo\
    bgpbgqbgrbgsbgtbgubgvbgwbgxbgybgzbhabhbbhcbhdbhebhfbhgbhhbhibhjbhlbhmbhn\
    bhobhpbhqbhrbhsbhtbhubhvbhwbhybhzbi biabibbidbiebifbigbikbilbimbinbiobip\
    biqbirbitbiubivbiwbiybizbjabjbbjcbjfbjgbjhbjibjjbjkbjlbjmbjnbjobjpbjrbjs\
    bjtbjubjvbjwbjxbjybjzbkabkcbkdbkfbkgbkhbkibkjbkkbklbkmbknbkobkpbkqbkrbks\
    bktbkubkvbkwbkxbkybkzblablbblcbldbleblfblhblibljblkblmblnbloblpblqblrbls\
    bltblvblwblxblyblzbm bmabmbbmcbmdbmebmfbmgbmhbmibmjbmkbmlbmmbmnbmobmpbmq\
    bmrbmsbmubmvbmwbmxbmzbn bnabnbbncbndbnebnfbngbnibnjbnkbnmbnnbnobnpbnqbnr\
    bnsbnubnvbnwbnxbnybnzbo boabobboebofbohbojbokbolbombonboobopboqborbotbou\
    bovbowboxboybozbpabpcbpdbpebpgbphbpibpjbpkbplbpmbpobppbpqbprbpsbptbpubpv\
    bpwbpxbpybpzbqabqbbqcbqdbqfbqgbqibqjbqkbqlbqmbqobqpbqqbqrbqsbqtbqubqvbqw\
    bqxbqzbr brabrbbrcbrdbrfbrgbrhbribrjbrkbrlbrmbrnbrobrpbrqbrrbrsbrtbrubrv\
    brwbrxbrybrzbs bsabsbbscbsebsfbshbsibsjbskbslbsmbsnbsobspbsqbsrbssbstbsu\
    bsvbswbsxbsybtabtcbtdbtebtfbtgbthbtibtjbtmbtnbtobtpbtqbtrbtsbttbtubtvbtw\
    btxbtybtzbuabubbucbudbuebufbugbuhbuibujbukbumbunbuobupbuqbusbutbuubuvbuw\
    buxbuybuzbvabvbbvcbvdbvebvfbvgbvhbvibvjbvkbvmbvnbvobvqbvrbvtbvubvvbvwbvx\
    bvybvzbwabwbbwcbwdbwebwfbwgbwhbwibwjbwkbwlbwmbwobwpbwqbwrbwsbwtbwubwwbwx\
    bwybwzbxabxbbxcbxfbxgbxhbxibxjbxlbxmbxnbxobxpbxqbxsbxubxvbxwbxzbyabybbyc\
    bydbyebyfbyhbyibyjbykbylbymbynbypbyrbysbyvbywbyxbyzbzabzbbzcbzdbzebzfbzh\
    bzibzjbzkbzlbzmbznbzobzpbzqbzrbztbzubzvbzwbzxbzybzzca caacabcaccadcaecaf\
    cagcahcajcakcalcamcancaocapcaqcarcascavcawcaxcaycazcbbcbccbdcbgcbicbjcbk\
    cblcbncbocbqcbrcbscbtcbucbvcbwcbycccccdcceccgcchccjcclccmccoccpccrcdecdf\
    cdhcdicdjcdmcdocdrcdzce ceacebcegcekcencetceycfacfdcfgcfmcgacgccggcgkch \
    chbchdchfchgchhchjchkchlchmchnchochpchqchrchtchwchxchychzciacibcicciecih\
    cimcincipcirciwciycjacjecjhcjicjkcjmcjncjocjpcjscjvcjyckbcklckmcknckockq\
    ckrckscktckuckvckxckyckzclaclccleclhclicljclkcllclmclocltcluclwclycmacme\
    cmgcmicmlcmocmrcmscmtcnacnbcnccngcnhcnicnkcnlcnpcnqcnscntcnwcnxco coacob\
    coccodcoecofcogcohcojcokcolcomcoocopcoqcotcoucoxcozcpacpbcpccpgcpicpncpo\
    cpscpucpxcpycqdcr cracrbcrccrdcrfcrgcrhcricrjcrkcrlcrmcrncrocrqcrscrtcrv\
    crwcrxcrycrzcs csacsbcshcsjcskcsmcsocspcsscstcsvcswcsycszctactcctdctectg\
    cthctlctmctnctoctpctscttctuctyctzcu cuacubcuccuhcuicujcukculcuocupcutcuu\
    cuvcuxcuycv cvgcvncwacwbcwecwgcwtcxhcy cyacybcyoczhczkczncztda daadacdad\
    daedagdahdaidajdakdaldamdaodaqdardasdaudavdawdaxdazdbadbbdbddbedbfdbgdbi\
    dbjdbldbmdbndbodbpdbqdbtdbudbvdbwdbydccdcrddadddddeddgddiddjddnddoddrdds\
    ddwde decdeddeedefdegdehdeidekdeldemdendeqderdesdevdezdgadgbdgcdgddgedgg\
    dghdgidgkdgldgndgrdgsdgtdgwdgxdgzdhgdhidhldhmdhndhodhrdhsdhudhvdhwdhxdia\
    dibdicdiddifdigdihdiidijdildindiodipdirdisdiudiwdixdiydizdjadjbdjcdjddje\
    djfdjidjjdjkdjmdjndjodjrdjudjwdkadkgdkkdkrdksdkxdlgdlmdlndmadmbdmcdmddme\
    dmfdmgdmkdmldmmdmodmrdmsdmudmvdmwdmxdmydnadnddnedngdnidnjdnkdnndnodnrdnt\
    dnudnvdnwdnydoadobdocdoedofdohdoidokdoldondoodopdordosdotdovdowdoxdoydpp\
    drcdredrgdridrldrndrodrqdrsdrtdrudrydsbdshdsidskdsndsodsqdtadtbdtddthdti\
    dtkdtmdtodtpdtrdtsdttdtudtyduadubducduedufdugduhduidukduldumdunduodupduq\
    durdusduuduvduwduxduyduzdv dvadwadwkdwrdwsdwudwwdwydwzdyadybdyddygdyidym\
    dyndyodyrdyudyydz dzadzddzedzgdzldzneaaebcebgebkeboebrebuecrecyee efaefe\
    efiegaeglegmegoegyehueipeiteivejaekaekeekgekieklekmekoekpekrekyel eleelk\
    elmeloeluemaembemeemgemiemmemnempemsemuemwemxemzen enaenbencendenfenhenl\
    enmennenoenqenrenvenwenxeo eotepieraergerherierkerrerserterwes eseesgesh\
    esiesmessesuesyet etbetnetoetretsettetuetxetzeu eudeveevhevnewoexteyaeyo\
    ezaezefa faafabfadfaffagfahfaifajfakfalfamfanfapfarfaufaxfayfazfblferff \
    ffiffmfgrfi fiafiefiffilfipfirfitfiwfj fkkfkvflaflhflifllflnflrflyfmpfmu\
    fnbfngfnifo fodfoifomfonforfosfpefqsfr frcfrdfrkfrmfrofrpfrqfrrfrsfrtfub\
    fudfuefuffuhfuifumfunfuqfurfutfuufuvfuyfvrfwafwefy ga gaagabgacgadgaegaf\
    gaggahgaigajgakgalgamgangaogapgaqgargasgatgaugawgaxgaygbagbbgbdgbegbfgbg\
    gbhgbigbjgbkgblgbmgbngbpgbqgbrgbsgbugbvgbwgbxgbygbzgccgcdgcfgclgcngcrgct\
    gd gdbgdcgddgdegdfgdggdhgdigdjgdkgdlgdmgdngdogdqgdrgdtgdugdxgeagebgecged\
    gefgeggehgeigejgekgelgeqgesgevgewgexgeygezgfkggaggbggdggegggggkgglggtggu\
    ggwghaghcgheghkghnghoghrghsghtgiagibgicgidgiegiggihgilgimgingipgiqgirgis\
    gitgixgiygizgjkgjmgjngjrgjugkagkdgkegkngkogkpgkugl glbglcgldglhgljglkgll\
    gloglrgluglwgmagmbgmdgmggmhgmlgmmgmngmrgmugmvgmxgmygmzgn gnagnbgncgndgne\
    gnggnhgnignjgnkgnlgnmgnngnqgnrgntgnugnwgnzgoagobgocgodgoegofgoggohgoigoj\
    gokgolgongoogopgoqgorgosgotgougovgowgoxgoygpagpegpngqagqngqrgragrbgrcgrd\
    grggrhgrigrjgrmgrqgrsgrtgrugrvgrwgrxgrygrzgslgsngsogspgswgtagtugu guagub\
    gucgudguegufguhguigukgulgumgunguogupguqgurgutguuguwguxguzgv gvagvcgvegvf\
    gvjgvlgvmgvngvogvpgvrgvsgvygwagwbgwcgwdgwegwfgwggwigwjgwmgwngwrgwtgwugww\
    gwxgxxgybgydgyegyfgyggyigylgymgyngyogyrgyygyzgzagzigznha haahachadhaehag\
    hahhaihajhakhalhamhanhaohaphaqharhashavhawhaxhayhazhbahbbhbnhbohbuhchhdy\
    he hedheghehheihemhgmhgwhhihhrhhyhi hiahibhidhifhighihhiihijhikhilhiohir\
    hithiwhixhjihkahkehkhhkkhlahlbhldhlthluhmahmbhmdhmfhmjhmmhmnhmphmqhmrhms\
    hmthmuhmvhmwhmyhmzhnahndhnehnghnhhnihnjhnnhnohnsho hoahobhochodhoehohhoi\
    hojholhomhoohophorhothovhowhoyhpohr hrahrchrehrkhrmhrohrphrthruhrwhrxhrz\
    hsbhsnhssht htihtohtshtuhtxhu hubhuchudhuehufhughuhhuihukhulhumhuphurhus\
    huthuuhuvhuwhuxhuyhuzhvchvehvkhvnhvvhwahwchwohy hyahywhz ia iaiianiariba\
    ibbibdibeibgibhiblibmibnibribuibyicaichicrid idaidbidciddideidiidridsidt\
    iduie ifaifbifeiffifkifmifuifyig igbigeiggigligmignigoigsigwihbihiihpihw\
    ii iinijcijeijjijnijsik ikhikiikkiklikoikpikriktikvikwikxikzilailbilgili\
    ilkilmiloilpiluilvimiimlimnimoimrimsimtimyin inbinginhinjinninoinpintio \
    ioriouiowipiipoiquiqwireirhiriirkirniruirxiryis isaiscisdishisiiskismisn\
    isoistisuit itbitditeitiitkitlitmitoitritsittitvitwitxityitziu iumivbivv\
    iw iwkiwmiwoiwsixcixliyaiyoiyxizhizmizrizzja jaajabjacjadjaejafjahjajjak\
    jaljamjanjaojaqjasjatjaujaxjayjazjbejbijbjjbkjbmjbnjbojbrjbtjbujbwjctjda\
    jdgjdtjebjeejehjeijekjeljenjerjetjeujgbjgejgkjgojhiji jiajibjicjidjiejig\
    jiljimjitjiujivjiyjjejjrjkajkmjkojkujlejmajmbjmcjmdjmijmljmnjmrjmsjmwjmx\
    jnajndjngjnijnjjnljnsjobjodjogjorjowjpajprjqrjrajrbjrrjrtjrujuajubjudjuh\
    juijukjuljumjunjuojupjurjutjuujuwjuyjv jvdjvnjw jwijyajyejyyka kaakabkac\
    kadkagkahkaikajkakkamkaokapkaqkavkawkaxkaykbakbbkbckbdkbekbgkbhkbikbjkbk\
    kblkbmkbnkbokbpkbqkbrkbskbtkbukbvkbwkbxkbykbzkcakcbkcckcdkcekcfkcgkchkci\
    kcjkckkclkcmkcnkcokcpkcqkcskctkcukcvkcwkcykczkdakdckddkdekdfkdgkdhkdikdj\
    kdkkdlkdmkdnkdpkdqkdrkdtkdwkdxkdykdzkeakebkeckedkeekefkegkehkeikekkelkem\
    kenkeokerkesketkeukevkewkexkeykezkfakfbkfckfdkfekffkfgkfhkfikfkkflkfmkfn\
    kfokfpkfqkfrkfskfukfvkfwkfxkfykfzkg kgakgbkgekgfkgjkgkkglkgokgpkgqkgrkgs\
    kgtkgukgvkgwkgxkgykhakhbkhckhdkhekhfkhgkhhkhjkhlkhnkhokhpkhqkhrkhskhtkhu\
    khvkhwkhxkhykhzki kiakibkickidkiekifkigkihkijkilkimkiokipkiqkiskitkiukiv\
    kiwkixkiykizkj kjakjbkjckjdkjekjgkjhkjikjjkjkkjlkjmkjnkjokjpkjqkjrkjskjt\
    kjukjxkjykjzkk kkakkbkkckkdkkekkfkkgkkhkkikkjkkkkklkkmkkokkpkkqkkrkkskkt\
    kkukkvkkwkkxkkykkzkl klaklbklckldkleklfklgklhklikljklkkllklmklnkloklpklq\
    klrklskltkluklvklwklxklyklzkm kmakmbkmckmdkmekmfkmgkmhkmikmjkmkkmlkmmkmn\
    kmokmpkmqkmskmtkmukmvkmwkmxkmykmzkn knaknbkndkneknfkniknjknkknlknmknnkno\
    knpknqknrknskntknuknvknwknxknyknzko koakockodkoekofkogkohkoikokkolkookop\
    koqkoskotkoukovkowkoykozkpakpckpdkpekpfkpgkphkpikpjkpkkplkpmkpnkpokpqkpr\
    kpskptkpukpwkpxkpykpzkqakqbkqckqdkqekqfkqgkqhkqikqjkqkkqlkqmkqnkqokqpkqq\
    kqrkqskqtkqukqvkqwkqxkqykqzkr krakrbkrckrdkrekrfkrhkrikrjkrkkrlkrnkrpkrr\
    krskrtkrukrvkrwkrxkrykrzks ksbkscksdkseksfksgkshksiksjkskkslksmksnksoksp\
    ksqksrksskstksuksvkswksxkszktaktbktcktdktektfktgkthktiktjktkktlktmktnkto\
    ktpktqktskttktuktvktwktxktyktzku kubkuckudkuekufkugkuhkuikujkukkulkumkun\
    kuokupkuqkuskutkuukuvkuwkuxkuykuzkv kvakvbkvckvdkvekvfkvgkvhkvikvjkvlkvm\
    kvnkvokvpkvqkvrkvtkvvkvwkvxkvykvzkw kwakwbkwckwdkwekwfkwgkwhkwikwjkwkkwl\
    kwmkwnkwokwpkwrkwskwtkwukwvkwwkwykwzkxakxbkxckxdkxfkxikxjkxkkxmkxnkxokxp\
    kxqkxrkxtkxvkxwkxxkxykxzky kyakybkyckydkyekyfkygkyhkyikyjkykkylkymkynkyo\
    kyqkyrkyskytkyukyvkywkyxkyykyzkzakzbkzckzdkzekzfkzikzkkzlkzmkznkzokzpkzr\
    kzskzukzvkzwkzxkzykzzla laalablacladlaelaglahlailajlallamlanlaplaqlarlas\
    laulawlaxlazlb lbblbelbflbilbjlbllbmlbnlbolbqlbrlbtlbulbvlbwlbxlbylbzlcc\
    lcdlcelcflchlcllcmlcplcqlcsldaldblddldgldhldildjldkldlldmldnldoldpldqlea\
    leblecledleeleflehleilejleklellemlenleolepleqlerlesletleulevlewlexleylez\
    lfalfnlg lgalgblgglghlgilgklgllgmlgnlgolgqlgrlgtlgulgzlhalhhlhilhmlhnlhs\
    lhtlhuli lialibliclidlielifliglihlijliklilliolipliqlirlisliulivliwlixliy\
    lizljaljeljiljlljpljwljxlkalkblkclkdlkelkhlkilkjlkllkmlknlkolkrlkslktlku\
    lkyllallbllclldllellfllgllilljllklllllmllnllpllqllullxlmalmblmclmdlmelmf\
    lmglmhlmilmjlmklmllmnlmolmplmqlmrlmulmvlmwlmxlmyln lnalnblndlnglnhlnilnj\
    lnllnmlnnlnslnulnwlnzlo loaloblocloeloglohloilojloklollomlonlooloploqlor\
    loslotloulowloxloylozlpalpelpnlpolpxlqrlralrclrglrilrklrllrmlrnlrolrtlrv\
    lrzlsalsdlselsilsmlsrlsslt ltcltglthltiltnltoltsltulu lualucludlueluflui\
    lujluklullumlunluolupluqlurluslutluuluvluwluyluzlv lvalvilvklvllvulwalwe\
    lwglwhlwllwmlwolwtlwwlxmlyalynlzhlzllznlzzmaamabmadmaemafmagmaimajmakmam\
    manmaqmasmatmaumavmawmaxmazmbambbmbcmbdmbfmbhmbimbjmbkmblmbmmbnmbombpmbq\
    mbrmbsmbtmbumbvmbwmbxmbymbzmcamcbmccmcdmcemcfmcgmchmcimcjmckmclmcmmcnmco\
    mcpmcqmcrmcsmctmcumcvmcwmcxmcymczmdamdbmdcmddmdemdfmdgmdhmdimdjmdkmdmmdn\
    mdpmdqmdrmdsmdtmdumdvmdwmdxmdymdzmeamebmecmedmeemehmejmekmelmemmenmeomep\
    meqmermesmetmeumevmewmeymezmfamfbmfcmfdmfemffmfgmfhmfimfjmfkmflmfmmfnmfo\
    mfpmfqmfrmftmfumfvmfwmfxmfymfzmg mgamgbmgcmgdmgemgfmggmghmgimgjmgkmglmgm\
    mgnmgomgpmgqmgrmgsmgtmgumgvmgwmgymgzmh mhbmhcmhdmhemhfmhgmhimhjmhkmhlmhm\
    mhnmhomhpmhqmhsmhtmhumhwmhxmhymhzmi miamibmicmidmiemifmigmihmiimijmikmil\
    mimminmiomipmiqmirmitmiumiwmixmiymizmjbmjcmjdmjemjgmjhmjimjjmjkmjlmjmmjn\
    mjqmjrmjsmjtmjumjvmjwmjxmjymjzmk mkamkbmkcmkemkfmkimkjmkkmklmkmmknmkomkp\
    mkrmksmktmkumkvmkwmkxmkymkzml mlamlbmlcmlemlfmlhmlimljmlkmllmlnmlomlpmlq\
    mlrmlsmlumlvmlwmlxmlzmmammbmmcmmdmmemmfmmgmmhmmimmmmmnmmommpmmqmmrmmtmmu\
    mmvmmwmmxmmymmzmn mnamnbmncmndmnemnfmngmnhmnimnjmnlmnmmnnmnpmnqmnrmnsmnu\
    mnvmnwmnxmnymnzmo moamocmodmoemogmohmoimojmokmommoomopmoqmormosmotmoumov\
    mowmoxmoymozmpampbmpcmpdmpempgmphmpimpjmpkmplmpmmpnmpomppmpqmprmpsmptmpu\
    mpvmpwmpxmpympzmqamqbmqcmqemqfmqgmqhmqimqjmqkmqlmqmmqnmqomqpmqqmqrmqsmqu\
    mqvmqwmqxmqymqzmr mramrbmrcmrdmrfmrgmrhmrjmrkmrlmrmmrnmromrpmrqmrrmrsmrt\
    mrumrvmrwmrxmrymrzms msbmscmsemsfmsgmshmsimsjmskmslmsmmsnmsomspmsqmssmsu\
    msvmswmsxmsymszmt mtamtbmtcmtdmtemtfmtgmthmtimtjmtkmtlmtmmtnmtomtpmtqmtr\
    mtsmttmtumtvmtwmtxmtymuamubmucmudmuemugmuhmuimujmukmummuomuqmurmusmutmuu\
    muvmuxmuymuzmvamvdmvemvfmvgmvhmvkmvlmvnmvomvpmvqmvrmvsmvtmvumvvmvwmvxmvy\
    mvzmwamwbmwcmwemwfmwgmwhmwimwkmwlmwmmwnmwomwpmwqmwrmwsmwtmwumwvmwwmwzmxa\
    mxbmxcmxdmxemxfmxgmxhmximxjmxkmxlmxmmxnmxomxpmxqmxrmxsmxtmxumxvmxwmxxmxy\
    mxzmy mybmycmyemyfmygmyhmyjmykmylmymmypmyrmyumyvmywmyxmyymyzmzamzdmzemzh\
    mzimzjmzkmzlmzmmznmzomzpmzqmzrmztmzumzvmzwmzxmzzna naanabnacnaenafnagnaj\
    naknalnamnannaonapnaqnarnasnatnawnaxnaynaznb nbanbbnbcnbdnbenbhnbinbjnbk\
    nbmnbnnbonbpnbqnbrnbtnbunbvnbwnbyncancbnccncdncencfncgnchncincjncknclncm\
    ncnnconcqncrnctncuncxncznd ndandbndcnddndfndgndhndindjndkndlndmndnndpndq\
    ndrndsndtndundvndwndxndyndzne neanebnecnedneenegnehneinejneknemnenneoneq\
    nernetneunewnexneyneznfanfdnflnfrnfung ngangbngcngdngenggnghngingjngkngl\
    ngmngnngpngqngrngsngtngungvngwngxngyngznhanhbnhcnhdnhenhfnhgnhinhknhmnhn\
    nhonhpnhqnhrnhtnhunhvnhwnhxnhynhznianibnidnienifnignihniinijnilnimninnio\
    niqnirnisnitniunivniwnixniyniznjanjbnjdnjhnjinjjnjlnjmnjnnjonjrnjsnjtnju\
    njxnjynjznkankbnkcnkdnkenkfnkgnkhnkinkjnkknkmnknnkonkqnkrnksnktnkunkvnkw\
    nkxnkznl nlanlcnlenlgnlinljnlknlmnlonlqnlunlvnlwnlxnlynlznmanmbnmcnmdnme\
    nmfnmgnmhnminmjnmknmlnmmnmnnmonmpnmqnmrnmsnmtnmunmvnmwnmxnmznn nnannbnnc\
    nndnnennfnngnnhnninnjnnknnlnnmnnnnnpnnqnnrnntnnunnvnnwnnynnzno noanocnod\
    noenofnognohnoinojnoknonnopnoqnosnotnounovnownoynpbnpgnphnplnpnnponpsnpu\
    npxnpynqgnqknqlnqmnqnnqonqqnqtnqynr nranrbnrenrfnrgnrinrknrlnrmnrnnrpnru\
    nrxnrznsansbnscnsdnsensfnsgnshnsknsmnsnnsonsqnssnstnsunsvnswnsxnsynszntd\
    ntentgntintjntkntmntontpntrntuntxntyntznuanucnudnuenufnugnuhnuinujnuknum\
    nunnuonupnuqnurnusnutnuunuvnuwnuxnuynuznv nvhnvmnvonwbnwcnwenwgnwinwmnwo\
    nwrnwwnwxnxanxdnxenxgnxinxlnxnnxonxqnxrnxxny nybnycnydnyenyfnygnyhnyinyj\
    nyknylnymnynnyonypnyqnyrnysnytnyunyvnywnyxnyynzanzbnzdnzinzknzmnzrnzunzy\
    nzzoaaoacoaroavobiobkoblobmoboobrobtobuoc ocaocoocuodaodkodtoduofsofuogb\
    ogcoggogooguohtohuoiaoieoinoj ojbojcojsojvojwokaokbokcokdokeokgokiokkokm\
    okookroksokuokvokxokzolaoldoleolkolmoloolroltoluom omaombomcomgomiomkoml\
    omoompomromtomuomwomxonaoneongonionjonkonnonoonponronsontonuonxoodoonoor\
    opaopkopmopooptopyor oraorcoreorgornoroorrorsortoruorvorworxorzos osaosc\
    osiosoospostosuosxotaotbotdoteotiotkotlotmotnotqotrotsottotuotwotxotyotz\
    ouboueouioumovdowiowloydoymoyyozmpa pabpacpadpaepafpagpahpaipakpalpampao\
    pappaqparpaspaupavpawpaxpaypazpbbpbcpbepbfpbgpbhpbipblpbmpbnpbopbppbrpbs\
    pbtpbvpbypcapcbpccpcdpcepcfpcgpchpcipcjpckpcmpcnpcppcwpdapdcpdnpdopdtpdu\
    peapebpedpeepegpeipekpelpempeopeppeqpevpexpeypezpfapfepflpgapgdpggpgipgk\
    pglpgnpgspguphdphgphhphkphlphmphnphophrphtphuphvphwpi piapibpicpidpifpig\
    pihpijpilpimpinpiopippirpispitpiupivpiwpixpiypizpjtpkapkbpkgpkhpknpkopkp\
    pkrpkupl plaplbplcpldpleplgplhplkpllplnploplrplspluplvplwplzpmapmbpmdpme\
    pmfpmhpmipmjpmlpmmpmnpmopmqpmrpmspmtpmwpmxpmypmzpnapncpndpnepngpnhpnipnj\
    pnkpnlpnmpnnpnopnppnqpnrpnspntpnvpnwpnypnzpocpoepofpogpohpoipokpomponpoo\
    poppoqpospotpovpowpoyppeppippkpplppmppnppopppppqppspptpqapqmpraprcprdpre\
    prfprgprhpriprkprmproprqprrprtpruprwprxps psapsepshpsipsmpsnpsqpsspstpsu\
    pswpt ptapthptiptnptoptpptrpttptuptvpuapubpucpudpuepufpugpuipujpumpuopup\
    puqpurputpuupuwpuxpuypwapwbpwgpwmpwnpwopwrpwwpxmpyepympynpyupyxpyypzepzh\
    pznqu quaqubqucqudqufqugquiqukqulqumqunqupquqqurqusquvquwquxquyqvaqvcqve\
    qvhqviqvjqvlqvmqvnqvoqvpqvsqvwqvzqwaqwcqwhqwmqwsqwtqxaqxcqxhqxlqxnqxoqxp\
    qxqqxrqxtqxuqxwqyaqypraarabracradrafragrahrairajrakramranraoraprarravraw\
    raxrayrazrbbrbkrblrbprcfrdbrearebreeregreirejrelremrenresretreyrgargnrgr\
    rgsrgurhgrhpriarifrilrimrinrirritriurjgrjirjsrkarkbrkhrkirkmrktrkwrm rma\
    rmbrmcrmdrmermfrmgrmhrmirmkrmlrmmrmnrmormprmqrmtrmurmwrmxrmzrn rndrngrnl\
    rnnrnrrnwro robrocrodroerofrogrolromrooroprorrourowrpnrptrrirrmrrorrtrsk\
    rswrtcrthrtmrtwru rubrucruerufrugruirukruorupruqrutruuruyruzrw rwarwkrwl\
    rwmrworwrrxdrxwryusa saasabsacsadsaesafsahsajsaksamsaosaqsarsassatsausav\
    sawsaxsaysazsbasbbsbcsbdsbesbgsbhsbisbjsbksblsbmsbnsbosbpsbqsbrsbssbtsbu\
    sbvsbwsbxsbysbzsc scbscescfscgschsciscksclscnscoscpscssctscuscvscwscxsd \
    sdasdbsdcsdesdfsdgsdhsdjsdksdnsdosdqsdrsdssdusdxse seasebsecsedseesefseg\
    sehseisejsekselsenseosepseqsersessetseusevsewseysezsfesfmsfwsg sgasgbsgc\
    sgdsgesghsgisgjsgmsgpsgrsgssgtsgusgwsgysgzshashbshcshdsheshgshhshishjshk\
    shmshnshoshpshqshrshsshtshushvshwshyshzsi siasibsidsiesifsigsihsiisijsik\
    silsimsipsiqsirsissiusivsiwsixsiysizsjasjbsjdsjesjgsjlsjmsjpsjrsjtsjusjw\
    sk skaskbskcskdskeskfskgskhskiskjskmsknskoskpskqskrskssktskuskvskwskxsky\
    skzsl slcsldslgslhslisljsllslmslnslpslrsluslwslxslyslzsm smasmbsmcsmfsmg\
    smhsmjsmksmlsmnsmpsmqsmrsmssmtsmusmwsmxsmysmzsn sncsnesnfsngsnisnjsnksnl\
    snmsnnsnosnpsnqsnrsnssnusnvsnwsnxsnysnzso soasobsocsodsoesogsoisoksolsoo\
    sopsoqsorsossousovsowsoxsoysozspbspcspdspespgspispksplspmspnsposppspqspr\
    spssptspvsq sqasqhsqmsqosqqsqtsqusr srasrbsresrfsrgsrhsrisrksrlsrmsrnsro\
    srqsrrsrssrtsrusrvsrwsrxsrysrzss ssbsscssdssessfssgsshssjsslssmssnssossq\
    ssssstssussvssxssysszst stastbstestfstgsthstistjstkstlstmstnstostpstqstr\
    stssttstvstwstysu suasubsucsuesugsuisujsuksuosuqsursussutsuvsuwsuysuzsv \
    svasvbsvcsvesvmsvssw swbswfswgswiswjswkswmswoswpswqswrswsswtswuswvswwswx\
    swysxbsxesxnsxrsxssxusxwsyasybsycsyisyksylsymsynsyosyrsyssywsyxszaszbszc\
    szgszlsznszpszvszwszyta taatabtactadtaetaftagtajtaktaltantaotaptaqtartas\
    tautavtawtaxtaytaztbatbctbdtbetbftbgtbhtbitbjtbktbltbmtbntbotbptbstbttbu\
    tbvtbwtbxtbytbztcatcbtcctcdtcetcftcgtchtcitcktcmtcntcotcptcqtcstcutcwtcx\
    tcytcztdatdbtdctddtdetdgtdhtditdjtdktdltdmtdntdotdqtdrtdstdttdvtdxtdyte \
    teatebtectedteetegtehteitektemtenteotepteqtertestetteutevtewtexteyteztfi\
    tfntfotfrtfttg tgatgbtgctgdtgetgftghtgitgjtgntgotgptgqtgstgttgutgvtgwtgx\
    tgytgzth thdthethfthhthithkthlthmthpthqthrthsthtthuthvthythzti tictiftig\
    tihtiitijtiktiltimtintiotiptiqtistittiutivtiwtixtiytjatjgtjitjjtjltjntjo\
    tjptjstjutjwtk tkatkbtkdtketkftkgtkltkptkqtkrtkstkttkutkvtkwtkxtkztl tla\
    tlbtlctldtlftlgtlitljtlktlltlmtlntlptlqtlrtlstlttlutlvtlxtlytmatmbtmctmd\
    tmetmftmgtmhtmitmjtmltmmtmntmotmqtmrtmttmutmvtmwtmytmztn tnatnbtnctndtng\
    tnhtnitnktnltnmtnntnotnptnqtnrtnstnttnvtnwtnxtnyto tobtoctodtoftogtohtoi\
    tojtoktoltomtootoptoqtortostoutovtowtoxtoytoztpatpctpetpftpgtpitpjtpktpl\
    tpmtpntpptprtpttputpvtpxtpytpztqbtqltqmtqntqotqptqttqutqwtr tratrbtrctre\
    trftrgtrhtritrjtrltrmtrntrotrptrqtrrtrstrttrutrvtrwtrxtrytrzts tsatsbtsc\
    tsdtsgtshtsitsjtsltsptsrtsttsutsvtswtsxtsztt ttbttcttdttettftthttittjttk\
    ttlttmttnttottpttrttstttttuttvttwttyttztuatubtuctudtuetuftugtuhtuitujtul\
    tumtuntuotuqtustuutuvtuxtuytuztvatvdtvetvitvktvltvmtvntvotvstvttvutvwtvx\
    twatwbtwdtwetwftwgtwhtwltwmtwntwotwptwqtwrtwttwutwwtwxtwytxatxetxgtxitxj\
    txmtxntxotxqtxstxttxutxxtxyty tyatyetyhtyityjtyltyntyptyrtystyttyutyvtyx\
    tyytyztzhtzjtzltzmtzntzotzxuamuarubaubiublubrubuubyudaudeudgudiudjudludm\
    uduuesufiug ugaugbugeughugouhauhnuisuivujiuk ukaukgukhukiukkukpukqukuukv\
    ukwukyulaulbulculeulfuliulkulmulnuluulwulyumaumbumdumgumiummumnumoumpumr\
    umsunauneunguniunkunmunnunrunuunxunzuonupiupvur uraurburcureurfurgurhuri\
    urkurmurnurourpurrurturuurvurwurxuryurzusaushusiuskuspussusuutauteuthutp\
    utrutuuumuuruveuvhuvluwauyauz uzsvaavaevafvagvahvaivajvalvamvanvaovapvar\
    vasvauvavvayvbbvbkve vecvemveovepvervgrvi vicvidvifvigvilvinvitvivvjkvka\
    vkjvkkvklvkmvknvkovkpvktvkuvkzvlpvlsvmavmbvmcvmdvmevmfvmgvmhvmivmjvmkvml\
    vmmvmpvmqvmrvmsvmuvmwvmxvmyvmzvnkvnmvnpvo vorvotvravrovrsvrtvtovumvunvut\
    vwawa waawabwacwadwaewafwagwahwaiwajwalwamwanwapwaqwarwaswatwauwavwawwax\
    waywazwbawbbwbewbfwbhwbiwbjwbkwblwbmwbpwbqwbrwbtwbvwbwwcawciwddwdgwdjwdk\
    wdtwduwdywecwedwegwehweiwemweowepwerweswetweuwewwfgwgawgbwggwgiwgowguwgy\
    whawhgwhkwhuwibwicwiewifwigwihwiiwijwikwilwimwinwirwiuwivwiywjawjiwkawkd\
    wkrwkwwkywlawlewlgwlhwliwlmwlowlrwlswluwlvwlwwlxwmawmbwmcwmdwmewmhwmiwmm\
    wmnwmowmswmtwmwwmxwnbwncwndwnewngwniwnkwnmwnnwnownpwnuwnwwnywo woawobwoc\
    wodwoewofwogwoiwokwomwonwooworwoswowwpcwrbwrgwrhwriwrkwrlwrmwrowrpwrrwrs\
    wruwrvwrwwrxwrzwsawsgwsiwskwsrwsswsuwsvwtbwtfwthwtiwtkwtmwtwwuawubwudwul\
    wumwunwurwutwuuwuvwuxwuywwawwbwwowwrwwwwxwwybwyiwymwynwyrwyyxaaxabxagxai\
    xajxakxalxamxanxaoxarxasxatxauxavxawxayxbbxbdxbexbgxbixbjxbmxbnxbpxbrxbw\
    xbyxchxcoxcrxdaxdkxdoxdqxdyxedxegxemxerxesxetxeuxgbxgdxggxgixgmxguxgwxh \
    xhexhmxhvxiixinxirxisxiyxjbxjtxkaxkbxkcxkdxkexkfxkgxkjxklxknxkpxkqxkrxks\
    xktxkuxkvxkwxkxxkyxkzxlaxlcxldxlyxmaxmbxmcxmdxmfxmgxmhxmjxmmxmnxmoxmpxmq\
    xmrxmtxmuxmvxmwxmxxmyxmzxnaxnbxnixnjxnkxnmxnnxnqxnrxntxnuxnyxnzxocxodxog\
    xoixokxomxonxooxopxorxowxpaxpbxpdxpfxpgxphxpixpjxpkxplxpmxpnxpoxpqxprxpt\
    xpvxpwxpxxpzxraxrbxrdxrexrgxrixrmxrnxrrxruxrwxsaxsbxsexshxsixsmxsnxspxsq\
    xsrxsuxsyxtaxtbxtcxtdxtexthxtixtjxtlxtmxtnxtpxtqxtsxttxtuxtvxtwxtyxubxud\
    xujxulxumxunxuoxutxuuxvexvixvnxvoxvsxwaxwdxwexwjxwkxwlxwoxwrxwtxwwxxbxxk\
    xxmxxrxxtxyaxybxyjxykxylxytxyyxzhxzpyaayabyacyadyaeyafyagyahyaiyajyakyal\
    yamyanyaoyapyaqyaryasyatyauyavyawyaxyayyazybaybbybeybhybiybjyblybmybnybo\
    ybxybyyclycnycrydaydeydgydkyeayecyeeyeiyejyelyeryesyetyeuyevyeyygaygiygl\
    ygmygpygryguygwyhdyi yiayigyihyiiyijyilyimyiryisyivykaykgykhykiykkykmyko\
    ykrykyylaylbyleylgyliyllylryluylyymbymeymgymkymlymmymnymoympynayndyngynk\
    ynlynqynsynuyo yobyogyoiyokyolyomyonyotyoyyrayrbyreyrkyrlyrmyroyrsyrwyry\
    ysdysnyspysryssysyytwytyyuayubyucyudyueyufyugyuiyujyulyumyunyupyuqyuryut\
    yuwyuxyuzyvayvtywaywgywnywqywrywuywwyxayxgyxlyxmyxuyxyyyryyuza zaazabzac\
    zadzaezafzagzahzajzakzamzaozapzaqzarzaszatzauzavzawzaxzayzazzbazbczbezbt\
    zbuzbwzcazchzdjzeazegzehzemzenzgazgbzghzgmzgnzgrzh zhdzhizhnzhwzhxziazik\
    zilzimzinziwzizzkazkdzkozkpzktzkuzkzzlazljzlmzlnzlqzluzmazmbzmczmdzmezmf\
    zmgzmhzmizmjzmkzmlzmmzmnzmozmpzmqzmrzmszmtzmuzmvzmwzmxzmyzmzznaznezngznk\
    znszoczohzomzoozoqzorzoszpazpbzpczpdzpezpfzpgzphzpizpjzpkzplzpmzpnzpozpp\
    zpqzprzpszptzpuzpvzpwzpxzpyzpzzqezrgzrnzrozrpzrszsazsrzsuzteztgztlztmztn\
    ztpztqztszttztuztxztyzu zuhzumzunzuyzwazygzyjzynzypzzazzj\
    ";

/// For each language of `LIKELY_LANG_KEYS`, its script in `LIKELY_SCRIPT_NAMES`.
pub(crate) static LIKELY_LANG_SCRIPTS_AT: &[u8] = &[
    55, 55, 55, 55, 55, 55, 67, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 31, 55, 55, 55, 55, 21, 55,
    55, 55, 55, 55, 55, 55, 2, 55, 88, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 2, 55, 55, 2, 55, 55, 55, 55, 55, 2, 2, 55, 55, 55, 55, 55, 55, 2, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 21, 55, 5, 55, 2, 2, 2, 55, 55, 55, 2, 55, 55,
    55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    22, 25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 25, 55, 55, 55, 55,
    55, 55, 1, 55, 22, 55, 55, 55, 2, 55, 55, 55, 55, 55, 100, 38, 55, 55, 55, 45, 72, 55, 2, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 119, 55, 55, 55, 55,
    55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 52, 67, 55, 55, 55, 55, 55, 21, 21,
    55, 25, 55, 55, 55, 25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 43, 55, 55,
    55, 100, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 22, 22, 22,
    55, 55, 25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 10,
    55, 55, 55, 2, 2, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 2, 3, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2,
    55, 2, 55, 55, 55, 2, 2, 10, 55, 55, 55, 94, 55, 55, 55, 55, 2, 55, 55, 55, 22, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 21,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2,
    21, 55, 2, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 25, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 4, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55,
    55, 2, 2, 55, 2, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55,
    55, 55, 55, 55, 2, 55, 55, 22, 55, 55, 55, 55, 55, 7, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 26, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 80, 55, 55, 55, 2, 21, 55, 55, 55, 55, 22, 55, 55,
    55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 105, 55, 2, 112, 80, 55, 22, 22, 21, 55, 55, 22, 22, 55, 55, 55, 55, 2, 55,
    2, 22, 55, 55, 55, 55, 55, 22, 31, 55, 55, 22, 22, 55, 22, 2, 55, 55, 21, 22, 22, 55, 2, 100,
    22, 55, 55, 55, 55, 22, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 100, 55, 55, 55, 22, 55, 55, 2, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 72, 55, 55, 55, 55, 55, 55, 55,
    107, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 10, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    22, 55, 55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 22, 10, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 22, 47, 55, 22, 55, 55, 2, 55, 55, 2, 55, 55, 55, 112, 55, 55, 55, 55, 55, 55, 52,
    49, 22, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 2, 55, 55, 55, 55, 55, 8, 55, 55, 25, 55,
    55, 55, 55, 55, 55, 55, 9, 55, 55, 55, 55, 55, 55, 9, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55,
    55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 72, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 69, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 22, 55, 55, 55, 55, 55, 25, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 13, 55, 108, 55,
    22, 32, 22, 22, 36, 55, 10, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 55,
    55, 55, 55, 2, 55, 55, 55, 55, 21, 55, 55, 55, 55, 17, 55, 55, 22, 55, 55, 55, 55, 55, 55, 22,
    55, 55, 55, 55, 55, 55, 2, 55, 55, 21, 55, 16, 55, 55, 55, 55, 55, 36, 2, 55, 55, 55, 55, 55,
    55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55,
    98, 55, 55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 55, 55, 36, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 111, 55, 55, 55, 55, 55, 55, 19, 55, 55, 55, 55, 55, 55, 55, 55, 31, 55, 55, 55,
    55, 55, 55, 55, 55, 14, 55, 55, 55, 55, 55, 55, 21, 55, 14, 14, 14, 14, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 72, 55, 55, 55, 55, 36, 55, 55, 55, 14, 55, 55, 55, 55, 83, 55, 10,
    55, 55, 55, 22, 55, 55, 55, 105, 55, 105, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 51,
    55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 36, 38, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55,
    55, 55, 55, 55, 55, 2, 55, 2, 55, 55, 55, 55, 55, 55, 10, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 32, 22, 55, 55, 55, 55, 22, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55,
    65, 55, 2, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55,
    72, 72, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 25, 55, 55,
    55, 112, 55, 55, 55, 55, 55, 22, 25, 55, 55, 22, 55, 55, 55, 55, 55, 80, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 32, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 22, 55, 55, 55, 55, 55, 55, 110, 55, 55, 80, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 55, 55, 55, 31, 20, 55, 55, 55,
    55, 55, 55, 55, 55, 23, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 44, 31, 55, 55,
    55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 21, 21, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 105, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 28, 2,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 41, 55, 55, 55, 55, 55, 21, 55, 21, 55, 55, 55, 55,
    55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 2, 55, 55, 55,
    55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 36, 55, 55, 80, 55, 32, 55, 108, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 80, 22, 32, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55,
    55, 80, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 22, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 25, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55,
    55, 2, 55, 22, 55, 55, 109, 2, 55, 112, 55, 55, 55, 55, 55, 2, 55, 55, 55, 21, 55, 55, 55, 55,
    55, 55, 55, 55, 2, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 2, 55, 2, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 53, 55, 55, 55, 55, 25, 55, 58, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 25, 55, 55, 55, 22,
    22, 55, 22, 55, 55, 55, 55, 55, 29, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 31, 55,
    55, 55, 55, 55, 55, 55, 55, 10, 25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 32, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 2, 55, 55, 2, 55, 55, 55, 55, 55, 55, 2, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 2, 55, 55, 55, 2, 55, 55, 55,
    55, 55, 55, 36, 55, 55, 55, 55, 55, 55, 25, 55, 55, 55, 55, 55, 2, 55, 55, 55, 38, 55, 55, 25,
    38, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 22, 55, 55, 102, 55, 55, 55, 55, 55,
    119, 55, 55, 55, 55, 55, 2, 55, 55, 22, 55, 55, 39, 55, 55, 86, 55, 11, 55, 55, 55, 11, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 2, 22, 55, 55, 55, 40, 55, 2, 55, 55, 55, 55, 22, 55, 55, 2, 55,
    22, 55, 55, 55, 55, 55, 55, 55, 35, 22, 72, 55, 55, 55, 55, 55, 55, 55, 55, 100, 55, 55, 55, 2,
    55, 36, 2, 55, 55, 55, 55, 55, 119, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    22, 55, 55, 55, 55, 38, 21, 55, 55, 55, 55, 55, 55, 55, 55, 4, 55, 4, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    120, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 60, 55, 55, 55, 21, 55, 55, 55, 55, 72, 55,
    25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 105, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 38, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 14, 55, 55, 55,
    38, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 43, 55, 55, 55, 2, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 38, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 21, 112,
    2, 21, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 26, 55, 55, 55, 38, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 34, 55, 55, 72, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55,
    102, 2, 55, 55, 55, 22, 22, 55, 55, 2, 55, 55, 38, 38, 55, 55, 38, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 22, 55, 80, 55, 55, 55, 55, 55, 55, 80, 55, 55, 55, 55, 55, 112, 38, 55, 26, 21, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 6, 55, 55, 55, 55, 55, 21, 55, 112, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 2, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 10, 55, 111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 21, 55, 67, 55, 22, 108, 55, 49, 22, 108, 49, 105, 55, 49, 67, 105, 22, 55, 2, 55,
    55, 22, 22, 22, 22, 22, 55, 55, 22, 22, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 22, 55, 104, 55, 55, 55, 111, 112, 55, 55, 55, 22, 12, 55, 55, 55, 55, 72, 55,
    21, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 21, 55, 22, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 52, 21, 55, 55, 55, 22, 55, 55, 22, 72, 55, 55, 55, 111,
    55, 55, 55, 112, 21, 55, 55, 55, 55, 55, 112, 55, 51, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55,
    22, 55, 55, 55, 55, 55, 55, 55, 55, 47, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 49, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 50, 55, 55, 55, 55, 55, 55, 55, 21, 22, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 21, 55, 55, 55, 21, 55, 55, 55, 55, 100, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 25, 55, 55, 22, 55, 21, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 47,
    55, 55, 22, 47, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 72, 55, 72, 55, 22, 55, 25, 55, 55, 22, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55,
    86, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 52, 55, 55, 55, 55, 55, 55, 21, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 72, 55, 72, 55, 55, 2, 44, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 72, 55, 55, 72, 111, 55, 55, 2,
    55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 44, 22, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 57, 55, 38, 22, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 21, 22, 55, 112, 55, 22, 55, 52, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 56, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 100,
    55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 59, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 22, 55, 55, 55, 55, 108, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 52, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 86, 55, 55, 55, 2, 55, 55, 2, 2, 55, 55, 55, 55, 55,
    55, 2, 38, 55, 55, 55, 55, 2, 55, 37, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 112, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 2, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 111, 111, 55, 55, 55, 55, 112, 55, 36, 55, 55, 55, 55, 55, 55, 55, 55, 22, 22, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 21, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 25, 25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 54, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 62, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55,
    67, 67, 55, 22, 108, 67, 55, 55, 55, 22, 21, 55, 22, 55, 22, 55, 2, 55, 55, 55, 111, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 67, 55, 55, 55, 55, 111, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 21, 55, 55, 69, 55, 55, 55, 55, 55, 10, 2, 55, 55, 55, 55, 55, 55, 21, 55,
    55, 72, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 22, 111, 55, 55, 22, 55, 55, 55, 21, 55, 55, 55, 55, 70, 55, 55, 22, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 22,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 22, 55,
    105, 55, 55, 25, 55, 55, 2, 69, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2,
    25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 72, 55, 55, 40, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 72, 55, 55, 55, 55, 55, 55, 55, 55, 55, 25, 55, 55, 55, 21, 55, 55, 55, 62, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 36, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 52, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 21, 112, 119, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 52, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21,
    55, 55, 55, 108, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 2, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 117, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 51,
    22, 55, 21, 55, 22, 55, 55, 90, 55, 55, 120, 55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 75, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 90, 55, 55,
    55, 55, 55, 55, 55, 120, 55, 120, 55, 55, 14, 55, 55, 55, 55, 55, 113, 55, 120, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 120, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 74, 55, 55, 55, 55, 55,
    55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 111, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 21, 21, 100, 26, 55, 55, 55, 85, 55, 72, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 119, 55, 55, 55, 55, 14, 55, 55, 14, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 34,
    35, 55, 55, 55, 55, 55, 47, 22, 55, 112, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55,
    55, 71, 68, 55, 55, 55, 72, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55,
    55, 55, 55, 55, 55, 55, 80, 55, 55, 55, 55, 55, 55, 55, 55, 108, 2, 21, 55, 55, 55, 21, 81, 41,
    42, 55, 55, 55, 55, 55, 2, 112, 55, 55, 55, 79, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 30, 55,
    55, 55, 82, 55, 55, 55, 55, 55, 55, 55, 55, 33, 55, 55, 55, 55, 55, 55, 55, 55, 55, 84, 55, 55,
    55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    2, 55, 55, 55, 47, 55, 55, 72, 67, 67, 22, 22, 108, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 80, 55, 55, 55, 55, 118, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 46, 22, 55, 55,
    77, 41, 55, 55, 22, 55, 55, 72, 2, 55, 85, 52, 2, 111, 111, 2, 22, 95, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 12, 55, 55, 55, 55, 55, 55,
    67, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 72, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 12, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 31, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 46, 2, 2, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 111, 55, 55, 2, 2, 55, 55, 2, 2, 55, 55, 55, 55, 2, 12,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 72, 22, 111, 55, 55, 55, 55, 55, 72, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    2, 55, 55, 55, 55, 55, 55, 22, 22, 55, 55, 22, 55, 10, 55, 22, 55, 55, 55, 55, 55, 55, 22, 55,
    55, 55, 55, 72, 55, 55, 55, 55, 2, 55, 55, 55, 55, 80, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 89, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 22, 47, 55, 55, 72, 55, 10, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 4, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 72, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21,
    55, 55, 55, 55, 22, 21, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 22, 55, 55, 45, 22, 55, 55, 55, 55, 55, 55, 21, 55, 55, 91, 55, 55, 55, 55, 78, 55, 55,
    55, 55, 55, 93, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 112,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 2, 55, 55, 22, 55, 52, 102, 55, 55, 31, 2,
    55, 2, 55, 55, 2, 2, 2, 55, 55, 55, 55, 55, 10, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 86, 55, 55, 55, 55, 55,
    55, 55, 21, 55, 22, 55, 55, 2, 55, 112, 55, 25, 2, 55, 55, 55, 55, 2, 55, 55, 55, 109, 55, 55,
    2, 72, 55, 55, 55, 55, 55, 55, 2, 2, 55, 55, 55, 95, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 112, 55, 55, 55, 55, 55, 55, 55, 2, 2, 55, 55, 21, 55, 55, 55, 55, 22, 55, 21, 55, 55,
    55, 55, 111, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    120, 55, 55, 55, 55, 91, 55, 55, 55, 55, 47, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 107, 55, 55, 55, 55, 96, 22, 55, 55, 55,
    55, 55, 55, 55, 111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 112, 80, 55, 55, 55, 55, 2, 52, 2, 55, 21, 55, 97, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 22, 55, 2, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55,
    52, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    2, 55, 25, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 99, 55,
    26, 55, 55, 55, 55, 55, 55, 2, 55, 55, 35, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55,
    55, 55, 55, 55, 55, 55, 90, 55, 55, 55, 100, 55, 55, 10, 55, 100, 55, 100, 55, 22, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 105, 55, 21, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 101, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 72, 55, 55, 55, 55, 55, 105,
    49, 55, 109, 22, 55, 103, 55, 22, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 108,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 42, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 21, 55, 55, 55, 55, 22, 112, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 111, 55, 22, 22, 55, 103, 55, 22, 111, 55, 22, 22, 22, 55, 55, 55, 55, 55, 25, 55, 55, 25,
    55, 55, 22, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 72, 55, 2,
    55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 2, 22, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 100, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 13, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 47, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55,
    55, 38, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55,
    31, 55, 55, 55, 112, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 52, 55, 55, 55,
    55, 55, 55, 52, 55, 55, 111, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 72, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 106, 55, 55,
    55, 55, 114, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 107, 55, 55, 55, 21, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 67, 21, 55, 55, 21,
    55, 55, 55, 2, 115, 55, 55, 21, 111, 55, 55, 55, 55, 55, 21, 55, 55, 55, 80, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 10, 55, 10, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55,
    111, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 31, 55, 55, 55, 55, 55, 55, 55, 2, 105, 55, 2, 55, 22, 116, 55, 55, 55, 55, 55, 55, 55,
    22, 55, 22, 22, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 49, 55, 55, 55, 2, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 25, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 55, 108, 22, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 25, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 27, 55, 55, 55, 55, 55, 2, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 36, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 0, 55,
    55, 55, 21, 55, 25, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 18, 15, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    2, 47, 55, 55, 55, 55, 80, 55, 55, 55, 2, 55, 2, 55, 55, 112, 55, 2, 55, 55, 2, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 60, 61, 24, 55, 55, 55, 55, 26, 55, 55, 55, 55, 63, 55, 55, 55,
    66, 55, 55, 55, 55, 55, 55, 55, 73, 55, 55, 55, 55, 55, 55, 55, 22, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 31, 55, 77, 55, 55, 55, 21, 55, 55, 55, 87, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 21, 55, 55, 55, 92, 55, 55, 55, 55, 55, 55, 55, 55,
    22, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 12, 55, 55, 55, 55, 55, 55, 105, 55,
    105, 55, 55, 55, 55, 55, 55, 41, 2, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 64, 55, 55, 55, 55, 55, 55, 55, 55, 55, 21, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 22, 22, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 2, 55, 67, 55, 55, 55, 31, 55, 55, 55, 55, 108, 55, 55, 55, 55, 55,
    55, 86, 55, 55, 55, 38, 38, 55, 120, 38, 55, 55, 55, 55, 55, 55, 120, 55, 21, 21, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 86, 55, 55, 21,
    55, 55, 55, 55, 55, 55, 55, 43, 55, 55, 55, 55, 55, 111, 55, 55, 55, 21, 55, 55, 55, 55, 55, 55,
    120, 120, 120, 21, 55, 86, 55, 55, 55, 55, 55, 38, 37, 55, 21, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 21, 55, 55, 55, 55, 55, 55, 86, 55, 86, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 112, 55, 55, 55, 55, 55, 2, 55, 55, 55,
    55, 55, 55, 35, 2, 55, 55, 35, 55, 109, 55, 35, 109, 35, 35, 55, 36, 35, 55, 55, 55, 76, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 21, 55, 48, 55, 21, 55, 35, 55, 35, 35, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 35, 80, 55, 55, 38, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    55, 55, 55, 55, 55, 55, 55, 55, 55, 2, 55, 55, 25, 35, 55, 35, 55, 55, 35,
];

/// For each language of `LIKELY_LANG_KEYS`, its region in `LIKELY_REGION_NAMES`.
pub(crate) static LIKELY_LANG_REGIONS_AT: &[u8] = &[
    53, 131, 131, 143, 143, 88, 84, 143, 143, 143, 143, 36, 22, 47, 22, 191, 188, 70, 143, 143, 80, 80, 62, 33,
    36, 144, 144, 28, 125, 143, 178, 33, 80, 131, 131, 131, 144, 64, 80, 143, 33, 17, 143, 144, 143, 80, 38, 131,
    64, 80, 103, 190, 85, 37, 131, 202, 71, 22, 133, 48, 191, 157, 139, 43, 160, 64, 180, 36, 176, 139, 9, 190,
    84, 33, 84, 80, 143, 64, 80, 9, 131, 22, 37, 155, 143, 86, 9, 182, 50, 2, 128, 36, 197, 145, 9, 37,
    143, 143, 143, 204, 99, 143, 131, 64, 143, 143, 131, 131, 143, 124, 64, 80, 141, 131, 131, 143, 143, 80, 143, 29,
    84, 53, 144, 143, 143, 144, 143, 36, 141, 36, 144, 71, 144, 158, 155, 144, 144, 64, 198, 53, 80, 33, 116, 176,
    33, 131, 84, 33, 84, 131, 191, 158, 37, 143, 9, 143, 143, 3, 85, 82, 131, 143, 84, 92, 84, 80, 2, 80,
    22, 53, 143, 30, 169, 19, 128, 9, 131, 84, 64, 80, 80, 131, 74, 131, 80, 143, 143, 85, 144, 168, 64, 143,
    198, 176, 143, 36, 155, 31, 191, 131, 35, 33, 191, 131, 9, 143, 144, 101, 84, 198, 201, 80, 80, 28, 155, 155,
    158, 53, 143, 9, 29, 53, 22, 131, 141, 141, 53, 9, 187, 175, 80, 143, 143, 131, 143, 80, 141, 92, 143, 124,
    80, 173, 9, 9, 9, 52, 38, 141, 131, 80, 128, 64, 60, 143, 155, 143, 131, 116, 84, 131, 38, 84, 84, 84,
    38, 9, 53, 36, 131, 143, 33, 143, 170, 143, 195, 143, 143, 143, 143, 9, 143, 128, 80, 143, 143, 198, 80, 13,
    74, 80, 158, 173, 160, 143, 144, 80, 135, 22, 191, 191, 191, 191, 22, 143, 198, 143, 143, 84, 22, 22, 191, 80,
    22, 143, 155, 115, 131, 131, 80, 144, 128, 151, 22, 50, 86, 9, 9, 38, 191, 22, 22, 141, 35, 21, 191, 47,
    22, 157, 22, 168, 22, 110, 50, 84, 188, 28, 80, 191, 131, 141, 80, 36, 2, 80, 22, 143, 84, 36, 52, 22,
    29, 143, 80, 80, 143, 37, 141, 144, 143, 131, 33, 28, 144, 144, 144, 86, 36, 144, 80, 22, 191, 144, 169, 155,
    191, 22, 198, 144, 158, 48, 158, 19, 205, 143, 109, 143, 198, 131, 143, 131, 143, 80, 143, 142, 80, 80, 143, 192,
    155, 143, 86, 33, 0, 50, 9, 64, 22, 141, 143, 169, 22, 84, 143, 131, 22, 9, 80, 143, 9, 143, 53, 131,
    80, 80, 22, 80, 80, 143, 143, 80, 7, 9, 22, 30, 9, 5, 128, 21, 143, 19, 141, 9, 131, 176, 202, 131,
    131, 109, 202, 151, 85, 143, 144, 144, 131, 80, 10, 86, 124, 124, 124, 124, 36, 144, 124, 155, 158, 73, 80, 195,
    36, 36, 23, 80, 145, 80, 38, 135, 8, 36, 131, 36, 36, 36, 80, 19, 143, 80, 143, 29, 143, 59, 36, 36,
    36, 62, 29, 143, 15, 30, 36, 143, 131, 131, 131, 143, 36, 36, 36, 37, 166, 80, 36, 143, 67, 143, 33, 9,
    9, 143, 131, 143, 29, 53, 28, 131, 29, 143, 131, 36, 131, 166, 166, 80, 38, 143, 131, 143, 125, 169, 160, 169,
    10, 80, 175, 36, 175, 188, 197, 125, 188, 30, 36, 84, 80, 80, 9, 145, 26, 28, 36, 36, 80, 84, 143, 19,
    80, 160, 143, 205, 143, 80, 31, 175, 33, 80, 33, 80, 169, 143, 188, 169, 84, 37, 36, 80, 30, 80, 143, 36,
    30, 36, 180, 15, 36, 84, 37, 145, 84, 84, 144, 84, 84, 16, 131, 80, 84, 84, 36, 84, 144, 36, 145, 67,
    145, 84, 84, 144, 158, 131, 80, 84, 184, 80, 80, 84, 84, 80, 84, 145, 143, 143, 82, 84, 135, 143, 139, 62,
    84, 80, 80, 112, 36, 84, 84, 80, 80, 29, 80, 198, 9, 15, 175, 143, 73, 143, 144, 131, 64, 131, 143, 29,
    143, 143, 143, 84, 64, 36, 84, 29, 29, 9, 143, 82, 73, 143, 53, 84, 143, 143, 85, 80, 30, 143, 143, 12,
    166, 36, 175, 33, 144, 9, 143, 131, 36, 144, 29, 30, 36, 198, 30, 84, 80, 36, 80, 36, 29, 22, 80, 144,
    29, 144, 131, 31, 180, 131, 80, 28, 158, 28, 80, 73, 80, 105, 29, 80, 116, 169, 144, 19, 158, 143, 37, 80,
    197, 6, 144, 144, 19, 80, 115, 131, 29, 143, 67, 30, 165, 29, 143, 175, 135, 143, 29, 112, 143, 36, 143, 115,
    38, 129, 143, 36, 31, 143, 143, 13, 80, 125, 144, 80, 80, 80, 69, 29, 144, 198, 69, 187, 144, 143, 80, 198,
    84, 80, 80, 143, 29, 125, 36, 37, 141, 93, 36, 15, 29, 143, 31, 131, 131, 143, 115, 143, 143, 22, 169, 188,
    64, 143, 15, 30, 115, 198, 36, 30, 143, 80, 155, 143, 29, 128, 9, 143, 80, 80, 80, 144, 144, 9, 143, 80,
    143, 84, 84, 80, 19, 80, 19, 36, 67, 176, 86, 166, 30, 143, 36, 36, 131, 80, 80, 143, 36, 29, 33, 131,
    131, 36, 58, 84, 95, 74, 135, 29, 21, 145, 36, 198, 160, 25, 29, 39, 24, 80, 143, 158, 80, 131, 197, 101,
    84, 84, 143, 143, 11, 80, 20, 166, 36, 131, 2, 36, 131, 145, 131, 80, 38, 175, 67, 105, 131, 36, 53, 80,
    67, 53, 131, 125, 131, 36, 80, 131, 175, 33, 125, 80, 80, 80, 144, 144, 143, 125, 198, 80, 131, 131, 145, 144,
    80, 80, 80, 155, 175, 203, 176, 28, 29, 80, 37, 31, 131, 143, 36, 165, 143, 80, 143, 131, 143, 29, 143, 59,
    131, 165, 131, 175, 69, 158, 158, 80, 175, 36, 131, 169, 131, 80, 36, 143, 175, 30, 9, 80, 80, 195, 131, 31,
    144, 80, 128, 55, 205, 143, 116, 143, 126, 36, 195, 15, 143, 29, 143, 53, 80, 15, 131, 29, 36, 64, 29, 37,
    15, 31, 158, 169, 69, 143, 29, 143, 9, 9, 15, 117, 9, 131, 36, 131, 36, 37, 175, 115, 143, 144, 36, 131,
    80, 143, 131, 135, 29, 131, 37, 80, 9, 51, 131, 143, 131, 36, 135, 143, 143, 105, 80, 112, 39, 115, 143, 143,
    177, 27, 132, 80, 29, 80, 29, 80, 80, 9, 0, 80, 36, 131, 115, 131, 131, 52, 71, 76, 71, 191, 166, 28,
    151, 141, 21, 71, 118, 128, 143, 21, 21, 84, 195, 21, 21, 21, 21, 28, 21, 38, 38, 38, 38, 48, 19, 144,
    116, 177, 131, 131, 141, 141, 141, 141, 38, 144, 38, 141, 22, 126, 131, 131, 73, 188, 125, 124, 13, 171, 84, 84,
    84, 84, 84, 135, 37, 131, 84, 155, 191, 144, 151, 116, 131, 131, 116, 131, 131, 131, 116, 143, 144, 190, 24, 72,
    38, 124, 124, 181, 191, 124, 56, 191, 155, 191, 191, 28, 124, 191, 141, 126, 135, 191, 124, 80, 19, 191, 131, 84,
    88, 22, 124, 128, 191, 195, 95, 197, 191, 155, 6, 197, 143, 141, 39, 155, 143, 37, 85, 131, 77, 116, 64, 175,
    143, 128, 155, 191, 187, 36, 131, 71, 131, 28, 124, 145, 64, 116, 84, 64, 191, 124, 116, 144, 155, 124, 197, 15,
    117, 38, 80, 197, 116, 88, 204, 84, 116, 197, 37, 116, 141, 116, 124, 37, 36, 80, 124, 116, 60, 58, 9, 124,
    124, 141, 38, 48, 177, 93, 124, 124, 191, 191, 28, 50, 191, 141, 166, 141, 124, 124, 141, 141, 70, 136, 64, 15,
    144, 141, 37, 141, 37, 28, 53, 194, 198, 191, 38, 28, 189, 170, 28, 28, 28, 28, 124, 191, 7, 159, 7, 84,
    197, 28, 131, 191, 44, 124, 146, 116, 116, 166, 191, 124, 37, 191, 191, 116, 28, 116, 191, 124, 191, 116, 124, 13,
    116, 124, 191, 135, 38, 124, 144, 84, 124, 84, 124, 155, 197, 38, 124, 93, 38, 141, 140, 22, 195, 191, 124, 37,
    36, 124, 124, 155, 84, 124, 188, 126, 188, 125, 166, 131, 60, 124, 21, 144, 37, 44, 124, 116, 46, 175, 143, 143,
    36, 64, 143, 175, 160, 191, 93, 131, 116, 84, 155, 33, 175, 93, 144, 9, 80, 115, 131, 131, 80, 80, 115, 131,
    125, 9, 131, 80, 131, 131, 36, 115, 115, 131, 115, 143, 84, 196, 9, 169, 31, 180, 143, 9, 19, 155, 9, 115,
    80, 45, 160, 143, 105, 86, 131, 145, 80, 36, 191, 80, 28, 30, 84, 22, 143, 29, 64, 115, 144, 15, 143, 143,
    131, 15, 30, 160, 9, 28, 15, 9, 9, 143, 143, 9, 135, 9, 6, 84, 84, 9, 188, 9, 128, 135, 9, 143,
    169, 33, 169, 9, 93, 124, 36, 80, 160, 169, 131, 169, 131, 84, 127, 169, 198, 80, 29, 9, 9, 175, 9, 129,
    9, 9, 9, 168, 115, 9, 80, 9, 143, 9, 24, 131, 80, 125, 169, 36, 155, 77, 84, 59, 115, 143, 9, 36,
    131, 125, 145, 145, 36, 36, 80, 80, 80, 125, 9, 126, 80, 80, 143, 188, 94, 80, 33, 80, 15, 29, 143, 80,
    116, 116, 80, 22, 143, 143, 37, 188, 143, 131, 84, 80, 143, 143, 29, 19, 158, 15, 131, 206, 36, 53, 64, 125,
    149, 135, 125, 131, 9, 80, 125, 135, 53, 133, 187, 135, 45, 93, 175, 131, 80, 84, 115, 37, 125, 28, 9, 115,
    115, 115, 115, 125, 125, 115, 115, 115, 135, 36, 84, 143, 144, 128, 93, 84, 143, 143, 144, 133, 80, 144, 80, 80,
    36, 135, 37, 80, 80, 115, 144, 36, 122, 143, 131, 84, 53, 0, 9, 143, 9, 135, 15, 9, 9, 144, 33, 115,
    9, 166, 131, 15, 9, 24, 131, 131, 9, 175, 24, 29, 9, 80, 131, 144, 31, 33, 93, 70, 43, 64, 131, 29,
    131, 33, 88, 188, 131, 50, 131, 80, 143, 143, 73, 131, 131, 80, 131, 13, 36, 126, 131, 131, 116, 70, 143, 143,
    131, 93, 143, 131, 80, 63, 135, 143, 124, 36, 140, 191, 84, 80, 58, 36, 191, 143, 93, 197, 80, 155, 155, 151,
    60, 131, 80, 143, 80, 131, 131, 151, 0, 33, 131, 84, 198, 131, 143, 198, 9, 37, 80, 80, 52, 21, 84, 86,
    191, 33, 191, 191, 144, 49, 131, 198, 36, 143, 131, 88, 131, 131, 80, 52, 124, 155, 131, 155, 36, 52, 191, 93,
    131, 131, 86, 143, 69, 143, 158, 143, 131, 143, 143, 36, 36, 131, 69, 166, 158, 80, 52, 86, 86, 144, 169, 166,
    143, 115, 175, 54, 160, 131, 157, 144, 188, 131, 161, 143, 55, 131, 134, 191, 80, 131, 36, 9, 29, 204, 36, 84,
    198, 204, 175, 57, 19, 143, 29, 19, 143, 187, 69, 143, 58, 191, 80, 45, 58, 58, 58, 143, 45, 45, 198, 36,
    199, 19, 67, 129, 175, 131, 22, 129, 88, 198, 29, 131, 143, 160, 128, 127, 133, 81, 64, 175, 84, 144, 195, 143,
    111, 143, 143, 143, 80, 180, 143, 37, 143, 143, 84, 143, 84, 143, 84, 143, 53, 80, 30, 9, 9, 143, 143, 30,
    19, 80, 84, 84, 84, 84, 169, 30, 30, 131, 19, 9, 30, 9, 19, 131, 86, 143, 9, 68, 61, 143, 63, 195,
    60, 84, 9, 143, 131, 131, 144, 9, 30, 9, 175, 53, 175, 143, 155, 202, 143, 9, 131, 84, 131, 143, 105, 131,
    80, 131, 28, 80, 176, 131, 131, 30, 80, 59, 131, 167, 29, 53, 143, 158, 105, 9, 9, 145, 9, 143, 143, 33,
    143, 109, 60, 135, 116, 158, 110, 145, 143, 135, 9, 131, 204, 36, 33, 145, 9, 96, 143, 155, 143, 197, 197, 36,
    28, 29, 9, 36, 145, 9, 64, 9, 145, 143, 143, 36, 131, 9, 67, 204, 52, 131, 175, 155, 2, 175, 86, 9,
    131, 105, 175, 131, 9, 158, 131, 143, 45, 45, 36, 36, 9, 143, 53, 188, 70, 131, 151, 15, 84, 52, 36, 131,
    176, 131, 9, 33, 25, 9, 143, 9, 125, 9, 143, 143, 21, 30, 33, 38, 143, 33, 24, 53, 188, 45, 143, 84,
    84, 105, 84, 55, 80, 80, 80, 133, 189, 36, 33, 188, 29, 175, 131, 64, 143, 131, 22, 175, 84, 105, 70, 131,
    143, 131, 158, 105, 125, 143, 80, 84, 53, 105, 143, 143, 105, 143, 166, 143, 30, 143, 32, 22, 9, 84, 131, 22,
    38, 33, 9, 9, 38, 21, 53, 191, 38, 22, 38, 9, 151, 64, 39, 195, 19, 15, 93, 83, 151, 22, 143, 143,
    22, 175, 131, 9, 22, 22, 135, 143, 9, 33, 131, 145, 53, 188, 145, 131, 28, 25, 9, 131, 190, 2, 9, 9,
    64, 33, 143, 9, 131, 9, 30, 36, 53, 140, 74, 135, 21, 9, 131, 160, 86, 80, 131, 191, 86, 80, 53, 64,
    143, 28, 84, 37, 197, 143, 188, 143, 80, 188, 53, 28, 29, 191, 28, 188, 2, 29, 131, 160, 82, 180, 124, 53,
    82, 175, 80, 188, 28, 29, 127, 143, 143, 166, 143, 84, 131, 141, 191, 55, 131, 143, 84, 36, 80, 144, 25, 22,
    184, 198, 22, 80, 188, 29, 84, 143, 143, 84, 197, 116, 184, 37, 115, 37, 197, 37, 37, 37, 37, 37, 84, 37,
    143, 80, 197, 37, 37, 37, 36, 145, 84, 6, 25, 37, 191, 144, 145, 168, 143, 158, 143, 84, 131, 131, 139, 191,
    84, 6, 169, 29, 191, 175, 143, 80, 37, 84, 116, 77, 84, 143, 197, 80, 37, 197, 9, 184, 84, 143, 22, 86,
    45, 37, 139, 78, 80, 38, 188, 80, 184, 79, 141, 25, 80, 124, 143, 141, 35, 143, 80, 143, 29, 191, 28, 124,
    135, 141, 124, 80, 141, 82, 155, 78, 124, 128, 80, 124, 33, 191, 131, 5, 36, 5, 127, 0, 128, 143, 143, 125,
    131, 9, 131, 144, 197, 144, 131, 131, 131, 80, 131, 19, 131, 38, 80, 93, 84, 131, 19, 131, 143, 169, 131, 180,
    131, 49, 144, 144, 176, 198, 144, 31, 144, 144, 131, 131, 131, 143, 131, 143, 21, 143, 0, 131, 80, 131, 80, 9,
    37, 9, 131, 131, 19, 131, 131, 191, 131, 131, 131, 131, 131, 131, 9, 28, 131, 131, 190, 188, 80, 205, 9, 37,
    144, 125, 144, 144, 80, 131, 143, 191, 143, 143, 80, 88, 169, 184, 80, 38, 191, 155, 38, 144, 143, 141, 116, 0,
    53, 143, 191, 143, 143, 141, 131, 80, 80, 131, 188, 22, 84, 80, 144, 87, 143, 141, 144, 131, 131, 2, 80, 188,
    131, 77, 36, 88, 144, 80, 21, 144, 88, 155, 131, 21, 143, 131, 144, 144, 131, 80, 144, 71, 28, 37, 144, 144,
    82, 144, 143, 80, 143, 124, 71, 131, 36, 31, 155, 131, 131, 131, 92, 22, 131, 71, 67, 143, 131, 125, 158, 125,
    80, 90, 9, 9, 80, 128, 2, 80, 80, 9, 128, 82, 9, 80, 143, 131, 109, 0, 80, 22, 36, 9, 189, 84,
    145, 155, 141, 135, 197, 80, 33, 80, 131, 131, 143, 175, 29, 62, 131, 36, 125, 189, 36, 131, 76, 131, 131, 9,
    143, 36, 188, 37, 48, 37, 98, 131, 80, 116, 143, 131, 160, 143, 131, 188, 80, 131, 135, 116, 64, 131, 143, 124,
    84, 145, 9, 131, 53, 84, 84, 29, 33, 145, 21, 115, 148, 82, 141, 197, 82, 131, 131, 195, 22, 131, 33, 131,
    9, 131, 135, 160, 84, 131, 22, 22, 46, 131, 131, 84, 80, 80, 168, 80, 64, 37, 82, 175, 62, 192, 47, 116,
    131, 125, 30, 131, 131, 144, 93, 115, 155, 141, 22, 80, 80, 22, 9, 22, 22, 155, 9, 84, 38, 80, 29, 143,
    175, 143, 30, 169, 176, 143, 53, 59, 143, 145, 80, 143, 143, 129, 131, 155, 143, 131, 80, 131, 131, 131, 131, 131,
    73, 206, 143, 30, 190, 143, 160, 131, 131, 143, 188, 29, 29, 47, 188, 9, 188, 9, 188, 143, 29, 176, 190, 190,
    128, 131, 131, 206, 131, 84, 106, 177, 80, 131, 80, 36, 41, 59, 160, 188, 191, 176, 160, 143, 80, 71, 29, 180,
    36, 190, 175, 131, 155, 176, 84, 143, 84, 84, 131, 84, 84, 84, 84, 84, 84, 84, 84, 84, 84, 36, 86, 36,
    33, 84, 84, 84, 84, 84, 84, 84, 84, 84, 15, 29, 33, 80, 80, 143, 135, 22, 9, 160, 22, 80, 80, 9,
    131, 143, 80, 80, 80, 135, 84, 37, 80, 80, 80, 101, 37, 80, 131, 143, 84, 86, 80, 115, 84, 143, 84, 6,
    155, 145, 29, 29, 143, 93, 175, 160, 191, 36, 175, 135, 80, 143, 143, 131, 155, 191, 135, 80, 143, 143, 184, 188,
    143, 84, 80, 188, 127, 80, 71, 80, 143, 80, 101, 155, 158, 10, 80, 135, 197, 9, 84, 116, 191, 80, 143, 177,
    191, 143, 143, 24, 100, 131, 80, 143, 131, 67, 84, 144, 116, 188, 36, 158, 80, 131, 160, 9, 29, 131, 131, 135,
    131, 80, 31, 80, 9, 28, 65, 191, 124, 36, 9, 135, 175, 144, 143, 80, 86, 131, 144, 143, 93, 131, 143, 143,
    135, 145, 143, 105, 198, 80, 143, 80, 80, 95, 64, 6, 37, 144, 36, 143, 143, 143, 131, 84, 144, 144, 84, 143,
    143, 36, 53, 143, 80, 143, 22, 29, 143, 131, 86, 84, 131, 144, 80, 144, 73, 131, 71, 165, 80, 22, 84, 165,
    36, 125, 143, 125, 22, 67, 143, 127, 80, 29, 15, 98, 143, 131, 80, 169, 131, 38, 31, 155, 84, 143, 190, 143,
    59, 56, 36, 175, 131, 131, 191, 143, 131, 38, 80, 105, 143, 56, 64, 80, 22, 131, 29, 197, 22, 176, 80, 143,
    80, 155, 80, 143, 143, 155, 190, 143, 143, 143, 85, 144, 143, 15, 188, 143, 143, 19, 143, 33, 205, 105, 175, 22,
    125, 67, 125, 204, 80, 143, 36, 53, 204, 131, 135, 191, 155, 180, 22, 198, 131, 165, 144, 155, 155, 105, 131, 95,
    169, 129, 84, 95, 105, 166, 10, 80, 84, 188, 144, 143, 143, 36, 158, 45, 143, 143, 191, 143, 131, 144, 131, 30,
    131, 143, 105, 15, 84, 29, 116, 80, 84, 197, 53, 131, 9, 135, 29, 9, 175, 80, 33, 143, 86, 143, 22, 143,
    37, 144, 80, 80, 29, 197, 191, 22, 29, 127, 184, 131, 80, 143, 143, 101, 131, 131, 22, 188, 80, 131, 155, 51,
    143, 143, 22, 64, 28, 191, 80, 30, 9, 9, 35, 155, 155, 80, 143, 80, 125, 175, 143, 80, 175, 36, 116, 36,
    38, 80, 80, 116, 80, 116, 80, 80, 145, 116, 80, 60, 22, 131, 31, 158, 80, 158, 175, 80, 38, 143, 28, 131,
    127, 127, 143, 33, 80, 29, 80, 36, 175, 168, 6, 6, 143, 33, 53, 20, 116, 125, 175, 116, 177, 125, 22, 145,
    80, 143, 143, 84, 143, 31, 197, 143, 94, 188, 144, 143, 80, 64, 33, 143, 191, 125, 144, 144, 191, 30, 144, 80,
    175, 22, 125, 80, 116, 135, 84, 143, 143, 22, 15, 80, 33, 80, 143, 80, 125, 158, 80, 80, 123, 59, 80, 36,
    125, 80, 80, 22, 80, 29, 80, 193, 144, 70, 124, 82, 84, 188, 145, 123, 190, 29, 205, 131, 175, 197, 64, 176,
    80, 80, 84, 143, 107, 143, 155, 84, 36, 84, 144, 84, 101, 101, 143, 135, 197, 143, 143, 80, 80, 9, 9, 80,
    80, 80, 80, 6, 80, 143, 37, 80, 80, 33, 131, 131, 131, 131, 31, 131, 131, 131, 67, 0, 131, 131, 131, 29,
    205, 21, 29, 15, 64, 205, 143, 29, 143, 29, 36, 171, 36, 84, 143, 143, 29, 143, 143, 80, 80, 80, 80, 155,
    36, 0, 190, 158, 158, 190, 197, 80, 198, 158, 29, 53, 169, 64, 158, 143, 158, 29, 197, 80, 37, 135, 125, 173,
    198, 37, 133, 165, 143, 37, 143, 29, 135, 64, 143, 88, 29, 28, 80, 64, 53, 105, 37, 160, 108, 80, 80, 30,
    29, 9, 80, 80, 80, 80, 9, 9, 180, 93, 197, 22, 190, 24, 86, 125, 143, 9, 198, 93, 169, 93, 191, 9,
    169, 131, 126, 67, 88, 143, 143, 80, 31, 9, 125, 143, 80, 175, 198, 80, 158, 55, 67, 198, 9, 160, 175, 80,
    143, 135, 29, 80, 84, 198, 84, 88, 36, 80, 80, 198, 55, 191, 36, 80, 29, 30, 127, 80, 79, 125, 143, 9,
    30, 143, 198, 36, 131, 9, 29, 101, 80, 15, 144, 80, 29, 169, 33, 143, 165, 29, 105, 123, 29, 131, 29, 33,
    143, 169, 191, 125, 80, 135, 205, 198, 80, 116, 37, 169, 169, 125, 86, 9, 93, 145, 86, 93, 80, 160, 80, 198,
    198, 86, 82, 29, 116, 190, 143, 145, 106, 37, 108, 190, 80, 22, 93, 93, 80, 29, 29, 190, 155, 205, 143, 191,
    29, 24, 169, 6, 205, 93, 59, 40, 80, 84, 191, 135, 139, 36, 93, 86, 108, 180, 101, 158, 29, 80, 29, 80,
    93, 197, 177, 37, 169, 80, 198, 143, 24, 205, 37, 198, 116, 184, 124, 124, 80, 131, 36, 84, 84, 124, 80, 71,
    66, 124, 93, 124, 124, 22, 64, 80, 124, 144, 144, 22, 144, 162, 143, 144, 22, 143, 22, 31, 38, 36, 38, 143,
    38, 144, 144, 131, 67, 143, 143, 145, 124, 151, 141, 143, 141, 124, 141, 195, 195, 143, 131, 6, 38, 125, 175, 124,
    36, 143, 143, 36, 36, 36, 143, 175, 30, 143, 143, 131, 143, 143, 36, 175, 155, 175, 144, 29, 29, 29, 29, 30,
    29, 29, 80, 143, 31, 31, 124, 31, 53, 53, 22, 36, 143, 9, 143, 143, 124, 80, 143, 125, 9, 165, 125, 9,
    36, 93, 175, 143, 143, 105, 131, 166, 191, 177, 80, 29, 36, 121, 36, 67, 36, 36, 36, 36, 131, 131, 131, 131,
    80, 176, 9, 143, 6, 166, 143, 53, 124, 169, 112, 81, 175, 169, 169, 175, 80, 36, 126, 131, 131, 80, 143, 180,
    30, 36, 135, 188, 205, 188, 143, 143, 188, 188, 188, 188, 113, 59, 124, 188, 125, 143, 9, 190, 2, 36, 143, 126,
    88, 205, 80, 191, 80, 195, 84, 25, 116, 80, 80, 138, 191, 124, 28, 85, 124, 36, 124, 124, 124, 36, 191, 124,
    124, 80, 124, 124, 132, 124, 124, 124, 143, 124, 124, 124, 180, 124, 191, 175, 37, 188, 37, 143, 143, 84, 143, 143,
    84, 84, 131, 84, 84, 84, 84, 13, 191, 135, 114, 33, 84, 143, 84, 131, 145, 56, 36, 19, 177, 80, 131, 143,
    143, 124, 128, 67, 198, 31, 144, 80, 180, 84, 198, 36, 197, 143, 101, 143, 80, 175, 93, 198, 158, 166, 143, 166,
    36, 160, 158, 198, 36, 198, 144, 131, 80, 124, 37, 198, 131, 198, 22, 143, 198, 144, 143, 143, 143, 37, 143, 36,
    22, 198, 143, 175, 29, 117, 143, 80, 37, 22, 175, 36, 197, 29, 84, 2, 198, 143, 197, 37, 125, 191, 155, 80,
    158, 116, 80, 126, 80, 153, 33, 7, 191, 28, 80, 28, 131, 31, 80, 132, 197, 27, 80, 160, 15, 38, 175, 191,
    31, 143, 53, 175, 188, 9, 9, 22, 53, 175, 9, 36, 9, 175, 143, 124, 143, 143, 143, 22, 158, 143, 143, 22,
    143, 22, 143, 80, 177, 80, 36, 80, 143, 80, 80, 124, 80, 80, 144, 19, 142, 80, 80, 80, 125, 80, 80, 169,
    143, 143, 80, 80, 143, 84, 177, 198, 191, 135, 80, 84, 84, 155, 128, 56, 198, 158, 13, 198, 142, 84, 198, 131,
    36, 142, 144, 80, 144, 80, 125, 144, 67, 175, 80, 80, 112, 125, 29, 144, 80, 144, 198, 80, 22, 128, 80, 143,
    36, 73, 143, 143, 143, 120, 144, 33, 143, 80, 158, 143, 80, 80, 143, 80, 36, 131, 155, 132, 124, 21, 197, 84,
    141, 198, 124, 143, 144, 124, 143, 36, 175, 36, 155, 48, 36, 169, 80, 175, 135, 143, 36, 37, 169, 191, 84, 93,
    84, 143, 36, 53, 143, 80, 145, 37, 124, 175, 143, 9, 143, 158, 80, 143, 80, 80, 198, 175, 125, 188, 80, 145,
    53, 143, 143, 143, 188, 9, 143, 143, 198, 115, 149, 175, 205, 198, 9, 116, 84, 93, 116, 169, 80, 191, 29, 124,
    124, 206, 80, 198, 36, 6, 29, 52, 84, 143, 19, 143, 80, 205, 124, 124, 125, 124, 124, 36, 124, 143, 33, 124,
    80, 116, 175, 29, 59, 53, 36, 191, 169, 115, 80, 53, 22, 141, 22, 155, 143, 190, 38, 86, 124, 36, 143, 7,
    124, 105, 131, 124, 131, 86, 22, 21, 80, 22, 125, 143, 30, 64, 74, 143, 136, 80, 22, 143, 80, 143, 84, 67,
    143, 143, 9, 37, 135, 88, 127, 131, 143, 131, 64, 143, 9, 124, 134, 6, 131, 84, 29, 84, 131, 84, 9, 143,
    30, 80, 131, 131, 80, 131, 84, 84, 36, 29, 143, 143, 84, 143, 135, 143, 143, 28, 124, 124, 124, 9, 124, 143,
    143, 143, 101, 36, 84, 64, 124, 191, 206, 31, 36, 126, 131, 155, 188, 188, 131, 188, 29, 29, 175, 31, 190, 6,
    131, 45, 29, 36, 166, 29, 80, 30, 169, 135, 80, 33, 80, 131, 128, 155, 24, 184, 143, 128, 128, 128, 197, 124,
    80, 143, 0, 135, 143, 33, 191, 80, 131, 158, 64, 36, 127, 29, 29, 29, 30, 36, 30, 204, 131, 36, 9, 126,
    56, 36, 188, 188, 158, 131, 101, 124, 36, 131, 131, 36, 31, 9, 33, 124, 151, 124, 9, 124, 124, 124, 124, 124,
    143, 124, 124, 25, 124, 36, 124, 124, 124, 124, 124, 80, 143, 9, 175, 143, 9, 188, 143, 80, 80, 188, 131, 155,
    93, 80, 143, 84, 137, 155, 143, 29, 29, 143, 131, 84, 188, 84, 9, 36, 169, 84, 84, 84, 131, 80, 168, 9,
    31, 36, 84, 205, 84, 36, 84, 158, 84, 143, 84, 84, 80, 198, 143, 6, 64, 64, 56, 80, 188, 33, 123, 29,
    131, 131, 133, 36, 80, 93, 158, 2, 29, 80, 145, 29, 116, 64, 124, 9, 84, 9, 158, 84, 198, 175, 59, 84,
    84, 36, 84, 131, 30, 198, 36, 135, 25, 84, 9, 206, 36, 198, 56, 191, 9, 143, 143, 176, 134, 9, 29, 175,
    198, 6, 143, 84, 36, 80, 53, 143, 84, 143, 175, 84, 188, 9, 191, 64, 9, 15, 9, 36, 134, 38, 143, 177,
    84, 143, 155, 143, 84, 38, 191, 161, 143, 29, 37, 141, 143, 0, 188, 175, 24, 116, 84, 124, 143, 84, 80, 84,
    158, 80, 19, 19, 6, 80, 143, 67, 116, 131, 116, 204, 59, 51, 84, 89, 198, 84, 9, 9, 125, 60, 88, 37,
    9, 143, 84, 204, 131, 37, 205, 37, 188, 36, 28, 84, 143, 204, 191, 143, 84, 124, 37, 198, 6, 80, 191, 125,
    126, 9, 15, 9, 188, 19, 29, 124, 64, 158, 116, 197, 86, 128, 22, 143, 29, 37, 9, 131, 69, 190, 28, 183,
    116, 197, 131, 143, 143, 169, 197, 29, 15, 56, 143, 9, 124, 191, 198, 143, 36, 33, 135, 36, 9, 198, 169, 9,
    143, 188, 135, 180, 29, 80, 80, 188, 80, 9, 59, 37, 143, 80, 123, 64, 29, 93, 6, 93, 29, 9, 160, 29,
    6, 177, 188, 190, 190, 190, 86, 123, 9, 9, 126, 9, 177, 9, 188, 36, 59, 29, 64, 30, 84, 131, 31, 175,
    115, 155, 155, 173, 62, 191, 144, 36, 91, 144, 116, 58, 131, 58, 141, 60, 124, 131, 145, 133, 131, 133, 131, 131,
    131, 131, 131, 131, 184, 79, 80, 169, 143, 28, 28, 28, 28, 158, 28, 28, 131, 29, 131, 131, 9, 93, 143, 98,
    98, 131, 131, 36, 143, 131, 95, 135, 188, 24, 9, 131, 155, 198, 106, 6, 53, 191, 198, 141, 141, 29, 155, 29,
    143, 84, 84, 93, 141, 143, 116, 7, 28, 143, 80, 143, 143, 143, 28, 84, 143, 143, 143, 198, 80, 191, 84, 204,
    131, 80, 143, 143, 124, 22, 84, 158, 93, 141, 131, 125, 143, 131, 125, 84, 145, 155, 22, 131, 80, 62, 191, 88,
    80, 131, 52, 36, 143, 45, 184, 37, 80, 124, 22, 117, 124, 124, 124, 124, 160, 124, 124, 22, 28, 124, 84, 124,
    105, 143, 37, 143, 161, 143, 60, 53, 22, 143, 36, 84, 22, 197, 22, 29, 22, 144, 22, 131, 22, 86, 144, 191,
    42, 178, 191, 80, 150, 22, 191, 22, 76, 22, 38, 74, 124, 124, 195, 195, 36, 131, 124, 131, 73, 67, 188, 124,
    2, 84, 143, 124, 95, 37, 58, 116, 84, 84, 84, 84, 84, 84, 131, 131, 21, 131, 143, 191, 80, 80, 28, 116,
    80, 191, 143, 80, 84, 124, 143, 80, 29, 86, 143, 191, 195, 143, 80, 125, 56, 36, 45, 169, 145, 84, 143, 198,
    81, 88, 131, 80, 84, 197, 197, 84, 145, 126, 102, 101, 145, 177, 177, 2, 135, 84, 124, 141, 59, 195, 56, 141,
    130, 38, 19, 191, 143, 38, 131, 22, 158, 9, 9, 158, 188, 143, 131, 128, 9, 84, 93, 143, 13, 9, 93, 34,
    84, 80, 146, 143, 198, 144, 60, 80, 7, 80, 145, 116, 38, 124, 33, 124, 22, 144, 144, 125, 198, 29, 9, 128,
    80, 84, 37, 37, 182, 36, 36, 80, 124, 143, 88, 142, 191, 84, 80, 124, 125, 80, 6, 125, 131, 34, 80, 9,
    21, 15, 125, 143, 141, 80, 15, 143, 80, 70, 9, 9, 36, 30, 71, 124, 29, 22, 71, 124, 22, 191, 56, 191,
    128, 124, 124, 191, 73, 124, 188, 143, 124, 80, 171, 80, 143, 143, 29, 143, 124, 143, 131, 28, 145, 2, 86, 170,
    144, 146, 144, 128, 116, 143, 58, 141, 22, 177, 80, 143, 84, 2, 80, 80, 2, 2, 21, 80, 143, 143, 145, 84,
    198, 22, 151, 22, 9, 80, 22, 143, 198, 80, 80, 198, 124, 84, 80, 80, 7, 80, 15, 38, 80, 135, 197, 143,
    21, 22, 80, 59, 56, 143, 191, 143, 131, 143, 144, 187, 116, 84, 177, 124, 33, 131, 22, 187, 116, 116, 131, 187,
    116, 141, 191, 141, 71, 48, 141, 48, 191, 141, 21, 71, 191, 141, 52, 141, 7, 71, 48, 141, 141, 141, 141, 141,
    141, 48, 48, 141, 141, 141, 141, 141, 141, 141, 48, 141, 141, 141, 79, 141, 191, 141, 141, 141, 48, 141, 141, 141,
    86, 48, 141, 141, 141, 0, 191, 135, 135, 80, 197, 135, 93, 84, 143, 84, 143, 22, 80, 143, 35, 34, 135, 116,
    131, 142, 80, 116, 144, 144, 9, 152, 86, 143, 80, 125, 188, 84, 80, 93, 141, 197, 131, 80, 21, 198, 88, 141,
    197, 80, 116, 143, 84, 110, 116, 188, 131, 80, 9, 80, 80, 135, 135, 95, 22, 34, 116, 15, 13, 9, 32, 132,
    9, 164, 46, 60, 54, 134, 80, 5, 143, 146, 80, 154, 32, 143, 52, 86, 161, 60, 197, 84, 18, 29, 126, 84,
    80, 9, 188, 153, 80, 197, 131, 143, 188, 197, 144, 153, 143, 9, 80, 175, 80, 198, 143, 158, 138, 143, 9, 154,
    131, 116, 80, 55, 84, 155, 190, 190, 189, 188, 158, 188, 131, 77, 153, 70, 155, 125, 131, 131, 156, 143, 188, 188,
    190, 143, 84, 9, 9, 92, 84, 175, 140, 191, 188, 22, 64, 155, 80, 59, 148, 80, 93, 21, 80, 84, 80, 166,
    80, 198, 131, 84, 175, 158, 143, 15, 143, 80, 143, 143, 175, 188, 144, 188, 145, 125, 188, 143, 80, 127, 80, 84,
    88, 59, 80, 205, 30, 88, 197, 37, 140, 80, 84, 104, 84, 145, 88, 60, 135, 28, 101, 84, 131, 131, 88, 145,
    80, 85, 88, 131, 85, 2, 86, 31, 143, 88, 125, 80, 13, 182, 80, 125, 134, 125, 33, 28, 197, 191, 33, 188,
    126, 124, 143, 28, 155, 15, 143, 15, 15, 191, 115, 80, 80, 33, 143, 48, 116, 144, 37, 64, 30, 81, 144, 93,
    144, 80, 178, 36, 84, 93, 84, 86, 106, 24, 80, 53, 2, 143, 131, 22, 29, 145, 53, 25, 191, 110, 160, 169,
    86, 116, 131, 141, 205, 29, 28, 191, 175, 139, 160, 47, 115, 104, 155, 125, 53, 205, 15, 64, 128, 84, 143, 22,
    64, 143, 84, 143, 131, 191, 143, 143, 143, 143, 86, 50, 38, 80, 155, 161, 175, 84, 144, 84, 143, 155, 161, 191,
    164, 191, 177, 143, 191, 198, 22, 112, 80, 80, 135, 143, 144, 80, 125, 15, 145, 143, 29, 198, 80, 74, 80, 158,
    80, 163, 38, 15, 80, 191, 146, 22, 143, 144, 191, 80, 37, 80, 143, 29, 80, 80, 200, 161, 143, 143, 143, 143,
    37, 161, 144, 144, 54, 82, 143, 80, 54, 84, 95, 80, 29, 86, 143, 206, 143, 125, 166, 29, 141, 30, 115, 144,
    190, 38, 191, 143, 59, 143, 198, 80, 125, 64, 143, 143, 143, 167, 177, 80, 29, 29, 29, 192, 135, 175, 143, 29,
    29, 143, 175, 15, 177, 150, 143, 36, 19, 188, 80, 195, 143, 143, 125, 80, 143, 143, 143, 151, 191, 115, 141, 80,
    143, 84, 84, 4, 131, 131, 30, 86, 101, 202, 28, 154, 143, 84, 80, 143, 144, 37, 38, 125, 80, 168, 168, 88,
    21, 166, 28, 80, 22, 144, 80, 84, 143, 86, 204, 144, 188, 143, 144, 187, 143, 1, 143, 64, 125, 93, 143, 80,
    101, 143, 143, 198, 143, 51, 143, 204, 205, 144, 80, 143, 197, 81, 197, 15, 143, 133, 143, 158, 28, 124, 45, 28,
    2, 197, 53, 56, 155, 80, 143, 29, 144, 143, 143, 143, 188, 188, 143, 53, 131, 67, 132, 84, 188, 22, 135, 161,
    62, 143, 194, 80, 88, 158, 188, 203, 29, 45, 37, 59, 123, 143, 22, 143, 36, 80, 80, 80, 80, 84, 198, 22,
    175, 93, 59, 80, 187, 131, 45, 19, 80, 144, 184, 59, 131, 13, 15, 86, 95, 85, 175, 135, 59, 125, 80, 125,
    29, 146, 80, 80, 36, 80, 187, 84, 191, 155, 124, 80, 22, 22, 160, 135, 131, 131, 131, 187, 29, 115, 124, 197,
    191, 38, 143, 175, 187, 160, 22, 143, 143, 158, 143, 143, 9, 160, 143, 144, 144, 29, 38, 143, 80, 143, 29, 124,
    143, 144, 143, 80, 19, 22, 191, 188, 64, 28, 124, 80, 174, 143, 59, 80, 135, 116, 116, 80, 9, 124, 124, 84,
    84, 84, 129, 84, 38, 37, 115, 135, 135, 80, 80, 131, 131, 74, 80, 131, 131, 197, 80, 180, 131, 112, 144, 84,
    125, 48, 93, 33, 124, 59, 7, 143, 29, 165, 38, 190, 124, 160, 22, 80, 180, 190, 80, 191, 169, 160, 129, 19,
    191, 80, 140, 80, 178, 93, 125, 143, 131, 135, 24, 185, 143, 84, 144, 143, 198, 125, 198, 144, 143, 22, 33, 28,
    169, 9, 177, 9, 135, 135, 124, 101, 93, 135, 177, 28, 135, 135, 135, 28, 169, 47, 131, 129, 53, 160, 143, 51,
    125, 29, 135, 36, 191, 143, 155, 143, 80, 15, 144, 38, 144, 131, 9, 191, 144, 105, 80, 37, 9, 116, 33, 47,
    9, 37, 9, 9, 181, 22, 84, 180, 126, 22, 112, 179, 158, 131, 10, 86, 135, 124, 143, 158, 80, 197, 144, 124,
    80, 124, 80, 143, 80, 191, 190, 80, 29, 198, 80, 124, 116, 158, 198, 80, 80, 80, 143, 10, 175, 198, 175, 143,
    22, 151, 80, 129, 198, 80, 80, 197, 80, 125, 143, 82, 198, 80, 29, 125, 143, 195, 204, 21, 38, 38, 38, 175,
    143, 80, 198, 198, 80, 198, 21, 198, 147, 166, 143, 80, 13, 80, 158, 188, 183, 7, 124, 67, 143, 123, 126, 205,
    124, 0, 191, 80, 124, 124, 169, 29, 124, 197, 86, 191, 150, 80, 36, 143, 124, 13, 80, 80, 143, 151, 22, 124,
    64, 22, 124, 22, 124, 95, 118, 124, 22, 143, 22, 198, 143, 191, 143, 143, 124, 158, 191, 184, 2, 143, 124, 80,
    185, 82, 143, 168, 175, 60, 2, 21, 84, 84, 124, 141, 124, 80, 184, 187, 145, 125, 84, 22, 204, 31, 53, 126,
    70, 144, 36, 28, 24, 197, 15, 198, 115, 187, 59, 131, 143, 124, 155, 131, 71, 143, 143, 36, 101, 80, 190, 38,
    205, 28, 80, 101, 80, 131, 177, 10, 143, 143, 125, 80, 135, 143, 191, 143, 22, 38, 38, 175, 143, 36, 80, 131,
    123, 191, 22, 175, 28, 191, 93, 22, 93, 15, 158, 131, 80, 131, 198, 186, 80, 116, 80, 93, 84, 36, 80, 187,
    191, 144, 133, 80, 191, 80, 197, 126, 84, 36, 25, 143, 129, 124, 22, 80, 143, 126, 80, 125, 80, 37, 22, 131,
    80, 80, 84, 80, 80, 80, 22, 125, 112, 142, 143, 131, 197, 31, 197, 197, 80, 9, 197, 197, 197, 25, 155, 31,
    131, 197, 124, 71, 0, 110, 80, 124, 143, 22, 143, 131, 175, 144, 143, 143, 184, 131, 155, 84, 155, 80, 36, 155,
    160, 80, 143, 37, 173, 9, 158, 155, 177, 131, 80, 143, 36, 131, 189, 80, 143, 30, 84, 116, 131, 131, 131, 169,
    131, 9, 131, 131, 155, 7, 80, 56, 9, 80, 143, 80, 132, 131, 191, 6, 9, 9, 125, 131, 116, 22, 9, 9,
    80, 143, 131, 9, 143, 22, 191, 9, 84, 143, 84, 80, 187, 143, 198, 145, 141, 22, 9, 21, 9, 143, 131, 143,
    177, 143, 80, 143, 22, 198, 143, 22, 143, 143, 143, 80, 22, 143, 145, 13, 36, 71, 131, 143, 131, 191, 131, 158,
    131, 143, 62, 198, 128, 143, 143, 9, 131, 192, 2, 84, 30, 86, 64, 84, 105, 127, 143, 143, 143, 198, 84, 124,
    84, 29, 84, 135, 80, 144, 204, 88, 131, 191, 155, 131, 145, 197, 172, 188, 31, 15, 7, 188, 131, 143, 84, 9,
    175, 80, 80, 22, 131, 80, 84, 80, 9, 131, 198, 14, 9, 9, 124, 84, 80, 45, 143, 86, 9, 124, 126, 9,
    124, 124, 124, 126, 80, 9, 126, 124, 124, 124, 158, 198, 198, 0, 131, 155, 198, 49, 158, 198, 80, 59, 188, 36,
    37, 14, 191, 143, 191, 80, 32, 22, 143, 80, 80, 143, 53, 191, 33, 74, 9, 144, 191, 143, 22, 131, 22, 143,
    168, 143, 195, 80, 80, 15, 188, 188, 188, 2, 145, 37, 9, 84, 84, 9, 9, 80, 22, 176, 59, 143, 9, 9,
    28, 9, 9, 33, 143, 9, 36, 143, 19, 80, 45, 143, 36, 80, 116, 80, 80, 9, 143, 9, 143, 80, 9, 9,
    80, 143, 80, 80, 15, 191, 9, 9, 9, 9, 143, 9, 9, 9, 9, 191, 22, 143, 143, 191, 131, 131, 188, 80,
    9, 9, 9, 143, 53, 9, 180, 80, 60, 80, 198, 199, 9, 7, 80, 64, 131, 9, 143, 22, 135, 180, 9, 80,
    128, 143, 80, 9, 126, 143, 143, 143, 9, 145, 80, 97, 80, 9, 9, 80, 143, 143, 191, 9, 166, 9, 33, 143,
    80, 56, 66, 143, 80, 36, 131, 29, 80, 80, 143, 80, 195, 9, 9, 9, 9, 9, 9, 9, 9, 80, 9, 143,
    80, 143, 9, 80, 9, 80, 84, 198, 143, 143, 64, 22, 2, 188, 143, 9, 53, 143, 84, 80, 9, 9, 176, 80,
    59, 188, 9, 143, 37, 143, 9, 80, 19, 9, 198, 9, 36, 9, 9, 9, 146, 191, 22, 55, 52, 131, 10, 22,
    22, 195, 155, 204, 53, 197, 143, 155, 22, 80, 22, 191, 80, 9, 9, 9, 9, 143, 9, 58, 125, 9, 80, 22,
    9, 191, 192, 184, 9, 9, 6, 155, 80, 36, 204, 80, 22, 143, 22, 143, 33, 9, 9, 9, 9, 9, 9, 204,
    145, 95, 197, 204, 71, 22, 84, 22, 9, 9, 145, 19, 86, 80, 80, 24, 115, 86, 80, 80, 86, 80, 22, 80,
    64, 31, 25, 80, 143, 125, 24, 143, 184, 184, 86, 167, 36, 126, 36, 62, 36, 9, 36, 80, 37, 22, 9, 9,
    160, 80, 9, 112, 112, 80, 9, 80, 157, 187, 9, 188, 9, 9, 144, 126, 84, 191, 9, 9, 50, 131, 80, 190,
    143, 22, 160, 64, 22, 143, 22, 143, 9, 9, 9, 9, 184, 9, 60, 9, 22, 9, 155, 22, 124, 191, 86, 9,
    9, 9, 9, 9, 22, 15, 9, 22, 9, 22, 155, 155, 88, 9, 143, 202, 144, 80, 131, 143, 64, 131, 143, 126,
    135, 195, 187, 124, 124, 160, 124, 80, 9, 124, 124, 124, 124, 124, 124, 86, 124, 124, 124, 9, 22, 124, 84, 9,
    84, 9, 88, 9, 175, 9, 127, 88, 2, 52, 88, 88, 22, 9, 19, 9, 9, 19, 155, 80, 9, 9, 64, 80,
    9, 22, 80, 9, 9, 9, 9, 22, 9, 9, 37, 124, 141, 22, 80, 141, 195, 29, 35, 178, 178, 30, 191, 67,
    36, 132, 126, 56, 124, 195, 36, 36, 195, 36, 22, 6, 131, 131, 131, 36, 37, 135, 135, 131, 131, 143, 22, 143,
    143, 143, 37, 38, 187, 9, 143, 145, 143, 84, 45, 143, 36, 70, 29, 131, 131, 80, 84, 143, 25, 9, 9, 143,
    143, 37, 143, 9, 143, 82, 189, 9, 37, 45, 9, 9, 9, 84, 80, 143, 37, 144, 155, 117, 80, 143, 143, 36,
    143, 30, 143, 143, 143, 143, 80, 143, 9, 143, 128, 143, 141, 29, 126, 143, 167, 80, 143, 143, 37, 9, 29, 155,
    143, 131, 29, 38, 131, 143, 144, 92, 191, 81, 29, 143, 131, 177, 143, 143, 33, 155, 22, 9, 22, 80, 143, 9,
    37, 37, 37, 155, 143, 37, 143, 9, 124, 9, 191, 82, 75, 191, 155, 38, 143, 30, 191, 131, 38, 21, 191, 143,
    143, 155, 21, 80, 195, 143, 9, 22, 37, 9, 37, 9, 9, 9, 9, 9, 9, 9, 9, 143, 37, 124, 124, 124,
    124, 124, 124, 160, 131, 188, 188, 124, 124, 124, 124, 124, 124, 124, 84, 124, 124, 124, 53, 131, 0, 125, 125, 80,
    131, 125, 124, 37, 97, 133, 143, 37, 131, 119, 188, 37, 110, 37, 37, 143, 37, 37, 131, 37, 36, 37, 143, 143,
    67, 175, 188, 188, 131, 80, 116, 155, 22, 37, 9, 155, 29, 37, 125, 37, 37, 131, 9, 29, 9, 9, 9, 29,
    9, 143, 125, 9, 9, 9, 9, 59, 160, 29, 29, 9, 29, 9, 9, 9, 29, 31, 9, 29, 175, 29, 197, 9,
    131, 124, 124, 84, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 124,
    124, 124, 124, 124, 124, 124, 124, 124, 124, 124, 37, 84, 175, 48, 58, 80, 143, 124, 143, 124, 124, 124, 124, 124,
    124, 124, 124, 124, 124, 124, 124, 204, 143, 139, 191, 36, 53, 37, 37, 37, 116, 184, 37,
];

/// The scripts `LIKELY_LANG_SCRIPTS_AT` indexes: four bytes each.
pub(crate) static LIKELY_SCRIPT_NAMES: &str = "\
    AghbAhomArabArmiArmnAvstBaliBamuBassBatkBengBopoBrahCakmCansCariChamCher\
    ChrsCoptCprtCyrlDevaEgypElymEthiGeorGongGonmGothGranGrekGujrGuruHangHani\
    HansHantHebrHluwHmnpItalJavaJpanKaliKanaKharKhmrKitsKndaKoreLanaLaooLatf\
    LatgLatnLepcLinaLinbLisuLyciLydiMandManiMarcMedfMercMlymModiMongMrooMtei\
    MymrNarbNewaNkooNshuOgamOlckOrkhOryaOsgeOugrPaucPhliPhnxPlrdPrtiRjngRohg\
    RunrSamrSarbSaurSgnwSinhSogdSoraSoyoSunuSyrcTagbTakrTaleTaluTamlTangTavt\
    TeluTfngThaaThaiTibtTnsaTotoUgarVaiiWchoXpeoXsuxYiii\
    ";

/// The regions `LIKELY_LANG_REGIONS_AT` indexes: three bytes each, space-padded.
pub(crate) static LIKELY_REGION_NAMES: &str = "\
    001AE AF AG AL AM AO AR AT AU AZ BA BB BD BE BF BG BH BI BJ BN BO BR BS \
    BT BW BY BZ CA CD CF CG CH CI CK CL CM CN CO CR CU CV CW CY CZ DE DK DZ \
    EC EE EG ER ES ET FI FJ FM FO FR GA GB GD GE GF GH GL GM GN GP GQ GR GT \
    GU GW GY HK HN HR HT HU ID IE IL IM IN IQ IR IS IT JE JM JO JP KE KG KH \
    KI KM KR KW KZ LA LB LC LK LR LT LU LV LY MA MD MG MH MK ML MM MN MP MR \
    MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG \
    PH PK PL PR PS PT PW PY RE RO RS RU RW SA SB SC SD SE SG SI SK SL SN SO \
    SR SS ST SV SX SY TC TD TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG US \
    UZ VA VC VE VI VN VU WF WS XK YE YT ZA ZM ZW \
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
