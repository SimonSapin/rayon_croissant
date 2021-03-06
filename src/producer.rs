// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use rayon::iter::plumbing::{Producer, ProducerCallback};
use std::mem;
use std::ops::Drop;

use Sink;

pub struct MapfoldReduceCallback<'t, Target: 't, Mapfold, Reduce, OutputCallback> {
    target: &'t mut Target,
    mapfold: Mapfold,
    reduce: Reduce,
    output_callback: OutputCallback,
}

struct MapfoldReduceProducer<
    't,
    Target: Default + 't,
    InputProducer,
    Mapfold,
    Reduce: Fn(&mut Target, Target),
> {
    sink: Sink<'t, Target>,
    part: Option<Part<'t, Target, Reduce>>,
    input_producer: InputProducer,
    mapfold: Mapfold,
    reduce: Reduce,
}

struct MapfoldReduceProducerIter<
    't,
    Target: Default + 't,
    Input,
    Mapfold,
    Reduce: Fn(&mut Target, Target),
> {
    front_sink: Sink<'t, Target>,
    back_sink: Target,
    part: Option<Part<'t, Target, Reduce>>,
    input_iter: Input,
    mapfold: Mapfold,
    reduce: Reduce,
}

enum Part<'t, Target: 't, Reduce: Fn(&mut Target, Target)> {
    Left(moite_moite::sync::Part<Option<Sink<'t, Target>>, Joiner<'t, Target, Reduce>>),
    Right(moite_moite::sync::Part<Option<Target>, Joiner<'t, Target, Reduce>>),
}

struct Joiner<'t, Target: 't, Reduce: Fn(&mut Target, Target)> {
    left: Option<Sink<'t, Target>>,
    right: Option<Target>,
    reduce: Reduce,
    parent: Option<Part<'t, Target, Reduce>>,
}

impl<'t, Target, Mapfold, Reduce, OutputCallback>
    MapfoldReduceCallback<'t, Target, Mapfold, Reduce, OutputCallback>
{
    pub fn new(
        target: &'t mut Target,
        mapfold: Mapfold,
        reduce: Reduce,
        output_callback: OutputCallback,
    ) -> Self {
        MapfoldReduceCallback {
            target,
            mapfold,
            reduce,
            output_callback,
        }
    }
}

impl<'t, Output, Target, Input, Mapfold, Reduce, OutputCallback> ProducerCallback<Input>
    for MapfoldReduceCallback<'t, Target, Mapfold, Reduce, OutputCallback>
where
    Output: Send,
    Target: Default + Send + 't,
    Input: Send,
    Mapfold: Clone + Fn(&mut Target, Input) -> Output + Send,
    Reduce: Clone + Fn(&mut Target, Target) + Send,
    OutputCallback: ProducerCallback<Output>,
{
    type Output = OutputCallback::Output;

    fn callback<P>(self, input_producer: P) -> Self::Output
    where
        P: Producer<Item = Input>,
    {
        self.output_callback.callback(MapfoldReduceProducer {
            sink: Sink::Borrowed(self.target),
            part: None,
            input_producer,
            mapfold: self.mapfold.clone(),
            reduce: self.reduce.clone(),
        })
    }
}

impl<'t, Output, Target, InputProducer, Mapfold, Reduce> Producer
    for MapfoldReduceProducer<'t, Target, InputProducer, Mapfold, Reduce>
where
    Output: Send,
    Target: Default + Send + 't,
    InputProducer: Producer,
    Mapfold: Clone + Fn(&mut Target, InputProducer::Item) -> Output + Send,
    Reduce: Clone + Fn(&mut Target, Target) + Send,
{
    type Item = Output;
    type IntoIter = MapfoldReduceProducerIter<'t, Target, InputProducer::IntoIter, Mapfold, Reduce>;

    fn into_iter(self) -> Self::IntoIter {
        MapfoldReduceProducerIter {
            front_sink: self.sink,
            back_sink: Target::default(),
            part: self.part,
            input_iter: self.input_producer.into_iter(),
            mapfold: self.mapfold,
            reduce: self.reduce,
        }
    }

    fn min_len(&self) -> usize {
        self.input_producer.min_len()
    }

    fn max_len(&self) -> usize {
        self.input_producer.min_len()
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let (left_input_producer, right_input_producer) = self.input_producer.split_at(index);
        let joiner = Joiner {
            left: None,
            right: None,
            reduce: self.reduce.clone(),
            parent: self.part,
        };
        let (left_part, right_part) =
            moite_moite::sync::split_with(joiner, |j| (&mut j.left, &mut j.right));
        let left_producer = MapfoldReduceProducer {
            sink: self.sink,
            part: Some(Part::Left(left_part)),
            input_producer: left_input_producer,
            mapfold: self.mapfold.clone(),
            reduce: self.reduce.clone(),
        };
        let right_producer = MapfoldReduceProducer {
            sink: Sink::Owned(Target::default()),
            part: Some(Part::Right(right_part)),
            input_producer: right_input_producer,
            mapfold: self.mapfold,
            reduce: self.reduce,
        };
        (left_producer, right_producer)
    }
}

impl<'t, Output, Target, Input, Mapfold, Reduce> Iterator
    for MapfoldReduceProducerIter<'t, Target, Input, Mapfold, Reduce>
where
    Output: Send,
    Target: Default + Send + 't,
    Input: Iterator,
    Mapfold: Clone + Fn(&mut Target, Input::Item) -> Output + Send,
    Reduce: Clone + Fn(&mut Target, Target) + Send,
{
    type Item = Output;

    fn next(&mut self) -> Option<Self::Item> {
        let input = self.input_iter.next()?;
        let output = (self.mapfold)(self.front_sink.as_mut(), input);
        Some(output)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.input_iter.size_hint()
    }
}

impl<'t, Output, Target, Input, Mapfold, Reduce> DoubleEndedIterator
    for MapfoldReduceProducerIter<'t, Target, Input, Mapfold, Reduce>
where
    Output: Send,
    Target: Default + Send + 't,
    Input: DoubleEndedIterator,
    Mapfold: Clone + Fn(&mut Target, Input::Item) -> Output + Send,
    Reduce: Clone + Fn(&mut Target, Target) + Send,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        let input = self.input_iter.next_back()?;
        let mut singleton = Target::default();
        let output = (self.mapfold)(&mut singleton, input);
        mem::swap(&mut self.back_sink, &mut singleton);
        (self.reduce)(&mut self.back_sink, singleton);
        Some(output)
    }
}

impl<'t, Output, Target, Input, Mapfold, Reduce> ExactSizeIterator
    for MapfoldReduceProducerIter<'t, Target, Input, Mapfold, Reduce>
where
    Output: Send,
    Target: Default + Send + 't,
    Input: ExactSizeIterator,
    Mapfold: Clone + Fn(&mut Target, Input::Item) -> Output + Send,
    Reduce: Clone + Fn(&mut Target, Target) + Send,
{
    fn len(&self) -> usize {
        self.input_iter.len()
    }
}

impl<'t, Target, Input, Mapfold, Reduce> Drop
    for MapfoldReduceProducerIter<'t, Target, Input, Mapfold, Reduce>
where
    Target: Default + 't,
    Reduce: Fn(&mut Target, Target),
{
    fn drop(&mut self) {
        let mut front_sink = mem::replace(&mut self.front_sink, Sink::Owned(Target::default()));
        let back_sink = mem::replace(&mut self.back_sink, Target::default());
        (self.reduce)(front_sink.as_mut(), back_sink);

        if let Some(split) = self.part.take() {
            split.commit(front_sink);
        } else if let Sink::Owned(_) = front_sink {
            panic!("front sink is owned and iter has no parent");
        }
    }
}

impl<'t, Target, Reduce> Drop for Joiner<'t, Target, Reduce>
where
    Target: 't,
    Reduce: Fn(&mut Target, Target),
{
    fn drop(&mut self) {
        let mut left_sink = self.left.take().expect("left was not committed");
        let right_sink = self.right.take().expect("right was not committed");

        (self.reduce)(left_sink.as_mut(), right_sink);

        if let Some(parent) = self.parent.take() {
            parent.commit(left_sink);
        } else if let Sink::Owned(_) = left_sink {
            panic!("left sink is owned and joiner has no parent");
        }
    }
}

impl<'t, Target, Reduce> Part<'t, Target, Reduce>
where
    Target: 't,
    Reduce: Fn(&mut Target, Target),
{
    fn commit(self, sink: Sink<'t, Target>) {
        match self {
            Part::Left(mut left) => {
                assert!(left.is_none(), "left was already committed");
                *left = Some(sink);
            }
            Part::Right(mut right) => {
                assert!(right.is_none(), "right was already committed");
                *right = Some(sink.into_owned());
            }
        }
    }
}
