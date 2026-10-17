//! Prediction: how Monkey's Audio turns decoded residuals back into samples. Each channel runs
//! the residual through a cascade of adaptive filters (more for higher compression levels), then
//! through a fixed-order predictor that also looks at the other channel.
//!
//! The arithmetic follows the reference encoder exactly, including where it wraps or saturates:
//! a single differing bit would throw every sample after it off.

/// Samples between two moves of a filter's history to the front of its buffer.
const NN_WINDOW: usize = 512;

/// An adaptive filter: predicts the next value from the last `order` ones with weights that
/// follow the sign of the error.
struct NnFilter {
    order: usize,
    shift: u32,
    /// Files before 3.98 adapt the weights less eagerly.
    old_adapt: bool,
    weights: Vec<i16>,
    /// Past outputs, saturated to 16 bits. The `order` values before `pos` are the history.
    input: Vec<i16>,
    /// How much each weight moves, per past output; same layout as `input`.
    delta: Vec<i16>,
    pos: usize,
    running_average: i32,
}

impl NnFilter {
    fn new(order: usize, shift: u32, version: u16) -> Self {
        Self {
            order,
            shift,
            old_adapt: version < 3980,
            weights: vec![0; order],
            input: vec![0; NN_WINDOW + order],
            delta: vec![0; NN_WINDOW + order],
            pos: order,
            running_average: 0,
        }
    }

    fn reset(&mut self) {
        self.weights.fill(0);
        self.input.fill(0);
        self.delta.fill(0);
        self.pos = self.order;
        self.running_average = 0;
    }

    fn decompress(&mut self, input: i32, interim: bool) -> i32 {
        let (pos, history) = (self.pos, self.pos - self.order);
        let dot = self.input[history..pos]
            .iter()
            .zip(&self.weights)
            .map(|(&x, &w)| i32::from(x) * i32::from(w))
            .fold(0i32, i32::wrapping_add);
        let round = 1 << (self.shift - 1);
        let prediction = if interim {
            ((i64::from(dot) + i64::from(round)) >> self.shift) as i32
        } else {
            dot.wrapping_add(round) >> self.shift
        };
        let output = input.wrapping_add(prediction);

        let deltas = &self.delta[history..pos];
        match input.signum() {
            -1 => self.weights.iter_mut().zip(deltas).for_each(|(w, &d)| {
                *w = w.wrapping_add(d);
            }),
            1 => self.weights.iter_mut().zip(deltas).for_each(|(w, &d)| {
                *w = w.wrapping_sub(d);
            }),
            _ => {}
        }

        // The step goes against the output's sign (the shifts pick out its sign bit), and is
        // larger for outputs far from the running average. Older steps fade.
        if self.old_adapt {
            self.delta[pos] = if output == 0 {
                0
            } else {
                (((output >> 28) & 8) - 4) as i16
            };
            self.delta[pos - 4] >>= 1;
            self.delta[pos - 8] >>= 1;
        } else {
            let abs = i64::from(output).abs();
            let average = i64::from(self.running_average);
            self.delta[pos] = if abs > average * 3 {
                ((output >> 25) & 64) - 32
            } else if abs > average * 4 / 3 {
                ((output >> 26) & 32) - 16
            } else if abs > 0 {
                ((output >> 27) & 16) - 8
            } else {
                0
            } as i16;
            self.running_average += ((abs - average) / 16) as i32;
            self.delta[pos - 1] >>= 1;
            self.delta[pos - 2] >>= 1;
            self.delta[pos - 8] >>= 1;
        }

        self.input[pos] = output.clamp(i16::MIN.into(), i16::MAX.into()) as i16;
        self.pos += 1;
        if self.pos == self.input.len() {
            self.input.copy_within(self.pos - self.order.., 0);
            self.delta.copy_within(self.pos - self.order.., 0);
            self.pos = self.order;
        }
        output
    }
}

/// Samples between two moves of the predictor's history to the front of its buffers.
const WINDOW: usize = 256;
const HISTORY: usize = 8;
const INITIAL_MA: [i32; 4] = [360, 317, -109, 98];

/// The predictor of one channel, for files from version 3.95 on.
pub struct Predictor {
    /// In the order they are applied: smallest first.
    filters: Vec<NnFilter>,
    /// Past values of this channel (`a`) and of the other (`b`), and the signs they adapt
    /// with. The `HISTORY` values before `pos` are the history.
    a: [i32; WINDOW + HISTORY],
    b: [i32; WINDOW + HISTORY],
    adapt_a: [i32; WINDOW + HISTORY],
    adapt_b: [i32; WINDOW + HISTORY],
    pos: usize,
    ma: [i32; 4],
    mb: [i32; 5],
    last_a: i32,
    /// The state of a first-order filter on the output, and of its inverse on `b`.
    filter_a: i32,
    filter_b: i32,
}

impl Predictor {
    pub fn new(compression: u16, version: u16) -> Self {
        let filters: &[(usize, u32)] = match compression {
            2000 => &[(16, 11)],
            3000 => &[(64, 11)],
            4000 => &[(32, 10), (256, 13)],
            5000 => &[(16, 11), (256, 13), (1280, 15)],
            _ => &[],
        };
        Self {
            filters: filters
                .iter()
                .map(|&(order, shift)| NnFilter::new(order, shift, version))
                .collect(),
            a: [0; WINDOW + HISTORY],
            b: [0; WINDOW + HISTORY],
            adapt_a: [0; WINDOW + HISTORY],
            adapt_b: [0; WINDOW + HISTORY],
            pos: HISTORY,
            ma: INITIAL_MA,
            mb: [0; 5],
            last_a: 0,
            filter_a: 0,
            filter_b: 0,
        }
    }

    /// Every frame starts from scratch.
    pub fn reset(&mut self) {
        self.filters.iter_mut().for_each(NnFilter::reset);
        for buf in [
            &mut self.a,
            &mut self.b,
            &mut self.adapt_a,
            &mut self.adapt_b,
        ] {
            buf.fill(0);
        }
        self.pos = HISTORY;
        self.ma = INITIAL_MA;
        self.mb = [0; 5];
        self.last_a = 0;
        self.filter_a = 0;
        self.filter_b = 0;
    }

    /// The next sample of this channel from its residual `residual` and the other channel's
    /// value `other`. `interim` repeats a rounding of an early 24-bit encoder.
    pub fn decompress(&mut self, residual: i32, other: i32, interim: bool) -> i32 {
        if self.pos == WINDOW + HISTORY {
            for buf in [
                &mut self.a,
                &mut self.b,
                &mut self.adapt_a,
                &mut self.adapt_b,
            ] {
                buf.copy_within(WINDOW.., 0);
            }
            self.pos = HISTORY;
        }

        let mut residual = residual;
        for filter in &mut self.filters {
            residual = filter.decompress(residual, interim);
        }

        let p = self.pos;
        self.a[p] = self.last_a;
        self.a[p - 1] = self.a[p].wrapping_sub(self.a[p - 1]);
        self.b[p] = other.wrapping_sub(scaled(self.filter_b));
        self.filter_b = other;
        self.b[p - 1] = self.b[p].wrapping_sub(self.b[p - 1]);

        let dot = |values: &[i32], weights: &[i32]| -> i64 {
            weights
                .iter()
                .enumerate()
                .map(|(i, &w)| i64::from(values[p - i]) * i64::from(w))
                .sum()
        };
        let (pred_a, pred_b) = (dot(&self.a, &self.ma), dot(&self.b, &self.mb));
        let prediction = if interim {
            ((pred_a + (pred_b >> 1)) >> 10) as i32
        } else {
            (pred_a as i32).wrapping_add((pred_b as i32) >> 1) >> 10
        };
        let current = residual.wrapping_add(prediction);

        // The weights move against the sign of the values they weigh, when the residual is
        // positive, and with it when negative.
        for i in [0, 1] {
            self.adapt_a[p - i] = -self.a[p - i].signum();
            self.adapt_b[p - i] = -self.b[p - i].signum();
        }
        let direction = -residual.signum();
        for (i, m) in self.ma.iter_mut().enumerate() {
            *m = m.wrapping_add(self.adapt_a[p - i] * direction);
        }
        for (i, m) in self.mb.iter_mut().enumerate() {
            *m = m.wrapping_add(self.adapt_b[p - i] * direction);
        }

        self.filter_a = current.wrapping_add(scaled(self.filter_a));
        self.last_a = current;
        self.pos += 1;
        self.filter_a
    }
}

/// `value * 31 / 32`, rounded down: the coefficient of the first-order filters.
fn scaled(value: i32) -> i32 {
    ((i64::from(value) * 31) >> 5) as i32
}
