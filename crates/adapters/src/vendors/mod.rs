//! Vendor modules (one per vendor, each behind its cargo feature).

#[cfg(feature = "alchemy")]
pub mod alchemy;
#[cfg(feature = "ankr")]
pub mod ankr;
#[cfg(feature = "birdeye")]
pub mod birdeye;
#[cfg(feature = "coingecko")]
pub mod coingecko;
#[cfg(feature = "cow")]
pub mod cow;
#[cfg(feature = "defillama")]
pub mod defillama;
#[cfg(feature = "dexscreener")]
pub mod dexscreener;
#[cfg(feature = "frankfurter")]
pub mod frankfurter;
#[cfg(feature = "geckoterminal")]
pub mod geckoterminal;
#[cfg(feature = "goplus")]
pub mod goplus;
#[cfg(feature = "helius")]
pub mod helius;
#[cfg(feature = "honeypot_is")]
pub mod honeypot_is;
#[cfg(feature = "jito")]
pub mod jito;
#[cfg(feature = "jupiter")]
pub mod jupiter;
#[cfg(feature = "moralis")]
pub mod moralis;
#[cfg(feature = "okx_dex")]
pub mod okx_dex;
#[cfg(feature = "oneinch")]
pub mod oneinch;
#[cfg(feature = "openexchangerates")]
pub mod openexchangerates;
#[cfg(any(feature = "flashbots", feature = "mev_blocker"))]
pub mod private_relay;
#[cfg(feature = "pyth")]
pub mod pyth;
#[cfg(feature = "quicknode_sol")]
pub mod quicknode_sol;
#[cfg(feature = "rugcheck")]
pub mod rugcheck;
#[cfg(feature = "trm")]
pub mod trm;
#[cfg(feature = "uniswap_api")]
pub mod uniswap_api;
#[cfg(feature = "velora")]
pub mod velora;
#[cfg(feature = "zeroex")]
pub mod zeroex;

pub(crate) mod util;

use bdm_config::Loaded;
use bdm_ports::Registration;

/// Registrations from every compiled-in vendor module (enhanced/REST APIs).
pub fn registrations(loaded: &Loaded) -> Vec<Registration> {
    #[allow(unused_mut)] // nothing is pushed when no vendor feature is compiled in
    let mut out = Vec::new();
    #[cfg(feature = "alchemy")]
    alchemy::register(loaded, &mut out);
    #[cfg(feature = "moralis")]
    moralis::register(loaded, &mut out);
    #[cfg(feature = "ankr")]
    ankr::register(loaded, &mut out);
    #[cfg(feature = "flashbots")]
    private_relay::register(loaded, &mut out, private_relay::FLASHBOTS);
    #[cfg(feature = "mev_blocker")]
    private_relay::register(loaded, &mut out, private_relay::MEV_BLOCKER);
    #[cfg(feature = "helius")]
    helius::register(loaded, &mut out);
    #[cfg(feature = "jito")]
    jito::register(loaded, &mut out);
    #[cfg(feature = "jupiter")]
    jupiter::register(loaded, &mut out);
    #[cfg(feature = "quicknode_sol")]
    quicknode_sol::register(loaded, &mut out);
    #[cfg(feature = "trm")]
    trm::register(loaded, &mut out);
    #[cfg(feature = "coingecko")]
    coingecko::register(loaded, &mut out);
    #[cfg(feature = "geckoterminal")]
    geckoterminal::register(loaded, &mut out);
    #[cfg(feature = "defillama")]
    defillama::register(loaded, &mut out);
    #[cfg(feature = "dexscreener")]
    dexscreener::register(loaded, &mut out);
    #[cfg(feature = "birdeye")]
    birdeye::register(loaded, &mut out);
    #[cfg(feature = "pyth")]
    pyth::register(loaded, &mut out);
    #[cfg(feature = "goplus")]
    goplus::register(loaded, &mut out);
    #[cfg(feature = "honeypot_is")]
    honeypot_is::register(loaded, &mut out);
    #[cfg(feature = "rugcheck")]
    rugcheck::register(loaded, &mut out);
    #[cfg(feature = "oneinch")]
    oneinch::register(loaded, &mut out);
    #[cfg(feature = "velora")]
    velora::register(loaded, &mut out);
    #[cfg(feature = "cow")]
    cow::register(loaded, &mut out);
    #[cfg(feature = "zeroex")]
    zeroex::register(loaded, &mut out);
    #[cfg(feature = "uniswap_api")]
    uniswap_api::register(loaded, &mut out);
    #[cfg(feature = "okx_dex")]
    okx_dex::register(loaded, &mut out);
    #[cfg(feature = "frankfurter")]
    frankfurter::register(loaded, &mut out);
    #[cfg(feature = "openexchangerates")]
    openexchangerates::register(loaded, &mut out);
    let _ = loaded;
    out
}
