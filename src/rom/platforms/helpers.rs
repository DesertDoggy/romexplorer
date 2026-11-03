use crate::common::models::Platform;

pub fn is_xbox_platform(platform: Platform) -> bool {
    match platform {
        Platform::Xbox | Platform::Xbox360 | Platform::XboxOne | Platform::XboxSeries => true,
        _ => false,
    }
}

pub fn is_playstation_platform(platform: Platform) -> bool {
    match platform {
        Platform::PlayStation | Platform::PlayStation2 | Platform::PlayStation3 | Platform::PlayStation4 | Platform::PlayStation5 | Platform::PlayStationPortable | Platform::PlayStationVita => true,
        _ => false,
    }
}

pub fn is_playstation_disc_platform(platform: Platform) -> bool {
    match platform {
        Platform::PlayStation | Platform::PlayStation2 | Platform::PlayStation3 | Platform::PlayStation4 | Platform::PlayStation5 | Platform::PlayStationPortable => true,
        _ => false,
    }
}

pub fn is_nintendo_platform(platform: Platform) -> bool {
    match platform {
        Platform::VirtualBoy | Platform::Nes | Platform::Famicom | Platform::FamicomDiskSystem| Platform::Snes | Platform::SufamiTurbo | Platform::Nintendo64 | Platform::Nintendo64Dd | Platform::GameCube | Platform::Wii | Platform::WiiU | Platform::GameBoy | Platform::GameBoyColor | Platform::GameBoyAdvance | Platform::NintendoDs | Platform::NintendoDsi | Platform::DsiWare | Platform::Nintendo3ds | Platform::NewNintendo3ds | Platform::NintendoSwitch | Platform::NintendoSwitch2=> true,
        _ => false,
    }
}

pub fn is_nintendo_disc_platform(platform: Platform) -> bool {
    match platform {
        Platform::GameCube | Platform::Wii | Platform::WiiU => true,
        _ => false,
    }
}

pub fn is_sega_platform(platform: Platform) -> bool {
    match platform {
        Platform::SegaMasterSystem | Platform::SegaGenesis | Platform::SegaCd | Platform::Sega32x | Platform::SegaSaturn | Platform::Dreamcast | Platform::SegaPico | Platform::Sg1000 => true,
        _ => false,
    }
}

pub fn is_sega_disc_platform(platform: Platform) -> bool {
    match platform {
        Platform::SegaCd | Platform::SegaSaturn | Platform::Dreamcast => true,
        _ => false,
    }
}

pub fn is_disc_console(platform: Platform) -> bool {
    is_playstation_disc_platform(platform)
        || is_nintendo_disc_platform(platform)
        || is_sega_disc_platform(platform)
        || matches!(platform, Platform::TurboGrafxCd | Platform::NeoGeoCd | Platform::ThreeDo | Platform::PcFx)
}

pub fn is_rvz_platform(platform: Platform) -> bool {
    match platform {
        Platform::GameCube | Platform::Wii => true,
        _ => false,
    }
}

pub fn is_chd_platform(platform: Platform) -> bool {
    match platform {
        Platform::PlayStation | Platform::PlayStation2 | Platform::PlayStationPortable | Platform::SegaCd | Platform::SegaSaturn | Platform::Dreamcast | Platform::TurboGrafxCd | Platform::NeoGeoCd | Platform::ThreeDo | Platform::PcFx => true,
        _ => false,
    }
}
