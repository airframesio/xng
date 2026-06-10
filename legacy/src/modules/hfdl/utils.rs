use std::collections::HashMap;

pub fn get_max_dist_khz_by_sample_rate(sample_rate: u32) -> u32 {
    (((sample_rate as f64) * 0.9) / 1000.0) as u32
}

pub fn freq_bands_by_sample_rate(freqs: &Vec<u16>, sample_rate: u32) -> HashMap<String, Vec<u16>> {
    let band_name = |band: &Vec<u16>| -> String {
        let first = *band.first().unwrap_or(&0);
        let last = *band.last().unwrap_or(&0);

        if first == 0 {
            return format!("{}", last);
        }
        if last == 0 || first == last {
            return format!("{}", first);
        }

        format!("{}-{}", first, last)
    };
    let mut bands: HashMap<String, Vec<u16>> = HashMap::new();

    let max_dist_khz = get_max_dist_khz_by_sample_rate(sample_rate) as u16;

    let mut band: Vec<u16> = Vec::new();
    for freq in freqs.iter() {
        match band.first() {
            Some(last_band) => {
                if freq - last_band > max_dist_khz {
                    bands.insert(band_name(&band), band.to_owned());
                    band.clear();
                }

                band.push(*freq);
            }
            None => band.push(*freq),
        }
    }

    if !band.is_empty() {
        bands.insert(band_name(&band), band.to_owned());
    }

    bands
}

pub fn first_freq_above_eq(freqs: &Vec<u16>, target_freq: u16) -> Option<u16> {
    freqs
        .iter()
        .position(|&x| x >= target_freq)
        .map(|i| freqs[i])
}
